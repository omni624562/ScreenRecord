//! 縮圖上停住就播放（像 YouTube 首頁）：滑鼠在縮圖上左右移動是滑過預覽，停住約 1 秒就在縮圖裡靜音播放，
//! 從停住的那個時間點開始；開始播之後在縮圖上移動也繼續播，離開縮圖才停止。最近錄影、全部錄影、Jellyfin 分頁共用。
//! 同時只播一支（FFmpeg 解成小畫面、不出聲音），所以狀態放在這裡，不用每個清單各記一份。

use super::theme;
use eframe::egui::{self, pos2, Color32, CornerRadius, Pos2, Rect, TextureHandle, TextureOptions};
use screenrecorder_core::player::Player;
use std::cell::RefCell;

/// 停住多久開始播（秒）
const DWELL: f64 = 1.0;
/// 游標移動超過這麼多（像素）算是又在滑過預覽
const MOVE: f32 = 6.0;
/// 解碼的最大尺寸（縮圖不大）
pub const DECODE_MAX: (u32, u32) = (480, 270);

#[derive(Default)]
struct State {
    /// 游標停在哪一張縮圖的哪裡、從什麼時候開始沒動
    still: Option<(String, Pos2, f64)>,
    playing: Option<Playing>,
    /// 最後一次有縮圖在問的那一格（沒人問就停掉）
    seen: u64,
}

struct Playing {
    key: String,
    player: Player,
    tex: Option<TextureHandle>,
    duration: f64,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
}

/// 每一格開始時：上一格沒有縮圖被滑鼠停著（移開、捲走、換頁）就停止播放
pub fn tick(ctx: &egui::Context) {
    let pass = ctx.cumulative_pass_nr();
    STATE.with_borrow_mut(|s| {
        if s.seen + 1 < pass && (s.playing.is_some() || s.still.is_some()) {
            s.playing = None;
            s.still = None;
        }
    });
}

/// 滑鼠在 rect（key 那張縮圖）上時每一格呼叫：停住夠久就用 start(從第幾秒) 開始播。
/// 回傳 true = 正在播（不要再浮出滑過預覽）
pub fn hover(ctx: &egui::Context, key: &str, rect: Rect, duration: f64, start: impl FnOnce(f64) -> Option<Player>) -> bool {
    let Some(pos) = ctx.pointer_hover_pos().filter(|p| rect.contains(*p)) else { return false };
    if !(duration.is_finite() && duration > 0.0) {
        return false;
    }
    let (now, pass) = (ctx.input(|i| i.time), ctx.cumulative_pass_nr());
    STATE.with_borrow_mut(|s| {
        s.seen = pass;
        // 正在播這一張：在縮圖上怎麼移動都繼續播，離開縮圖才停（tick）
        if s.playing.as_ref().is_some_and(|p| p.key == key) {
            return true;
        }
        let moved = !matches!(&s.still, Some((k, p, _)) if k == key && p.distance(pos) < MOVE);
        if moved {
            s.still = Some((key.to_string(), pos, now));
            // 移到另一張縮圖：前一張停掉
            s.playing = None;
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(DWELL));
            return false;
        }
        let since = s.still.as_ref().map_or(now, |x| x.2);
        if now - since < DWELL {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(DWELL - (now - since)));
            return false;
        }
        // 從停住的時間點開始；太靠近結尾就從頭
        let t = start_at(((pos.x - rect.min.x) / rect.width().max(1.0)) as f64, duration);
        let Some(player) = start(t) else {
            // 播不了：這次停住不再試
            s.still = Some((key.to_string(), pos, f64::INFINITY));
            return false;
        };
        player.seek(t);
        player.play();
        s.playing = Some(Playing { key: key.to_string(), player, tex: None, duration });
        true
    })
}

/// 滑鼠還在縮圖上、但在縮圖上的按鈕上（勾選、刪除…）：不開始播，正在播的繼續播
pub fn keep(ctx: &egui::Context, key: &str) {
    let pass = ctx.cumulative_pass_nr();
    STATE.with_borrow_mut(|s| {
        if s.playing.as_ref().is_some_and(|p| p.key == key) {
            s.seen = pass;
        }
    });
}

/// 停在縮圖的 frac（0～1）處：從那一秒開始播；離結尾不到 1.5 秒就從頭
fn start_at(frac: f64, duration: f64) -> f64 {
    let t = frac.clamp(0.0, 1.0) * duration;
    if t > duration - 1.5 {
        0.0
    } else {
        t
    }
}

/// key 正在播：把目前的畫面畫在 rect（和縮圖一樣填滿）和下緣的進度條，回傳 true；還沒有畫面時畫一個轉圈
pub fn paint(painter: &egui::Painter, key: &str, rect: Rect, rounding: u8) -> bool {
    STATE.with_borrow_mut(|s| {
        let Some(pl) = s.playing.as_mut().filter(|p| p.key == key) else { return false };
        let ctx = painter.ctx();
        if let Some(f) = pl.player.take_frame() {
            let img = egui::ColorImage::from_rgba_unmultiplied([f.width as usize, f.height as usize], &f.rgba);
            match &mut pl.tex {
                Some(t) => t.set(img, TextureOptions::LINEAR),
                None => pl.tex = Some(ctx.load_texture("hover-play", img, TextureOptions::LINEAR)),
            }
        }
        // 播完了從頭再播
        if pl.player.take_ended() {
            pl.player.seek(0.0);
            pl.player.play();
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(33));
        let Some(tex) = &pl.tex else {
            painter.rect_filled(rect, CornerRadius::same(rounding), Color32::from_black_alpha(120));
            let a = (ctx.input(|i| i.time) * 4.0) as f32;
            let c = rect.center();
            let pts: Vec<Pos2> = (0..=16).map(|k| a + k as f32 * 0.3).map(|t| c + 9.0 * egui::vec2(t.cos(), t.sin())).collect();
            painter.add(egui::Shape::line(pts, egui::Stroke::new(2.0, Color32::WHITE)));
            return true;
        };
        // 和縮圖一樣填滿、裁掉多的部分（開始播放時畫面不會跳）
        theme::paint_thumb_cover(painter, rect, tex);
        // 下緣的進度條
        let frac = (pl.player.time() / pl.duration.max(0.001)).clamp(0.0, 1.0) as f32;
        let bar = Rect::from_min_max(pos2(rect.min.x, rect.max.y - 3.0), rect.max);
        painter.rect_filled(bar, CornerRadius::ZERO, Color32::from_white_alpha(70));
        painter.rect_filled(Rect::from_min_max(bar.min, pos2(bar.min.x + bar.width() * frac, bar.max.y)), CornerRadius::ZERO, theme::pal_ctx(ctx).accent);
        true
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_where_the_mouse_rests() {
        assert_eq!(start_at(0.0, 60.0), 0.0);
        assert_eq!(start_at(0.5, 60.0), 30.0);
        // 停在最右邊（結尾）：從頭播
        assert_eq!(start_at(1.0, 60.0), 0.0);
        assert_eq!(start_at(2.0, 60.0), 0.0);
    }
}
