//! 擷取範圍的預覽：即時模式由 FFmpeg 持續輸出 RGBA 畫面（平常每秒 5 張；切到其他視窗或錄影中 2 張；
//! 大小跟著預覽實際顯示的像素），否則（或即時預覽失敗時）只擷取一張。視窗隱藏時停止。

use eframe::egui::{self, Color32, ColorImage, TextureHandle, TextureOptions};
use screenrecorder_core::app::{App, LIVE_MAX_WIDTH};
use screenrecorder_core::tr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;

/// 即時預覽失敗後先用單張，過這麼久再試一次（例如 UAC 安全桌面、鎖定畫面時 ddagrab 暫時不能用）
const LIVE_RETRY: Duration = Duration::from_secs(30);
/// 只有張數或大小改變時，維持這麼久才重開 FFmpeg（拖曳視窗大小、切換視窗時不會一直重開）
const SETTLE: Duration = Duration::from_millis(600);
/// 即時預覽寬度的級距（像素）
const WIDTH_STEP: u32 = 160;

#[derive(Debug, Clone, PartialEq)]
pub enum PreviewState {
    Idle,
    Loading { live: bool },
    Ready,
    Failed(String),
}

#[derive(Default)]
struct Slot {
    frame: Option<ColorImage>,
    seq: u64,
    state: Option<PreviewState>,
    /// 即時預覽失敗的時間：之後一段時間改用單張
    live_failed: Option<Instant>,
}

/// 要執行的預覽：範圍（"" 整個桌面或螢幕 id）、每秒張數（0 = 單張）、即時預覽的寬度
#[derive(Debug, Clone, PartialEq)]
struct Want {
    key: String,
    fps: u32,
    width: u32,
}

pub struct Preview {
    /// 目前執行中的預覽；None = 需要重新開始
    current: Option<Want>,
    /// 只有張數或大小改變：等穩定後才重開
    pending: Option<(Want, Instant)>,
    slot: Arc<Mutex<Slot>>,
    stop: Arc<AtomicBool>,
    texture: Option<TextureHandle>,
    tex_seq: u64,
    pub size: Option<(u32, u32)>,
}

impl Default for Preview {
    fn default() -> Self {
        Preview { current: None, pending: None, slot: Arc::default(), stop: Arc::new(AtomicBool::new(false)), texture: None, tex_seq: 0, size: None }
    }
}

/// 即時預覽的寬度：預覽在畫面上實際佔的像素（以 WIDTH_STEP 分級，320 到 LIVE_MAX_WIDTH）。
/// 不多擷取看不到的像素，也省掉縮小時的鋸齒
pub fn live_width(points: f32, pixels_per_point: f32) -> u32 {
    let px = (points * pixels_per_point).ceil().max(1.0) as u32;
    (px.div_ceil(WIDTH_STEP) * WIDTH_STEP).clamp(2 * WIDTH_STEP, LIVE_MAX_WIDTH)
}

impl Preview {
    /// 下次畫面時重新開始（換了範圍、重新偵測、開始 / 結束錄影）
    pub fn reset(&mut self) {
        self.current = None;
        self.pending = None;
    }

    /// 重新嘗試即時預覽
    pub fn retry_live(&mut self) {
        self.slot.lock().unwrap().live_failed = None;
        self.reset();
    }

    /// 即時預覽先前失敗過才重試（例如 ddagrab 的測試結果出來了：測試期間用 ddagrab 開可能失敗）
    pub fn retry_if_failed(&mut self) {
        if self.slot.lock().unwrap().live_failed.take().is_some() {
            self.reset();
        }
    }

    pub fn stop(&mut self, core: &App) {
        self.stop.store(true, Ordering::Relaxed);
        core.stop_live();
        self.reset();
    }

    /// 預覽在執行中（或已要求執行）
    pub fn running(&self) -> bool {
        self.current.is_some()
    }

    pub fn state(&self) -> PreviewState {
        self.slot.lock().unwrap().state.clone().unwrap_or(PreviewState::Idle)
    }

    /// 確認預覽在執行（key = "" 整個桌面或螢幕 id；fps = 0 單張；width = 即時預覽的寬度）
    #[allow(clippy::too_many_arguments)]
    pub fn ensure(&mut self, core: &Arc<App>, rt: &tokio::runtime::Handle, ctx: &egui::Context, key: &str, mut fps: u32, width: u32, ffmpeg_ok: bool) {
        if !ffmpeg_ok {
            self.slot.lock().unwrap().state = Some(PreviewState::Failed(tr!("需要 FFmpeg 才能顯示預覽", "FFmpeg is needed to show the preview").into()));
            return;
        }
        if self.slot.lock().unwrap().live_failed.is_some_and(|t| t.elapsed() < LIVE_RETRY) {
            fps = 0;
        }
        let want = Want { key: key.to_string(), fps, width: if fps > 0 { width } else { 0 } };
        if self.current.as_ref() == Some(&want) {
            self.pending = None;
            return;
        }
        // 同一個範圍、已經有畫面，只是張數或大小不同：穩定一段時間才重開 FFmpeg
        let same_key = self.current.as_ref().is_some_and(|c| c.key == want.key);
        if same_key && fps > 0 && self.state() == PreviewState::Ready {
            let since = match &self.pending {
                Some((p, t)) if *p == want => *t,
                _ => {
                    self.pending = Some((want, Instant::now()));
                    ctx.request_repaint_after(SETTLE);
                    return;
                }
            };
            if since.elapsed() < SETTLE {
                ctx.request_repaint_after(SETTLE - since.elapsed());
                return;
            }
        }
        self.pending = None;
        self.current = Some(want);
        self.stop.store(true, Ordering::Relaxed);
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = stop.clone();
        {
            let mut s = self.slot.lock().unwrap();
            if !same_key {
                // 換了範圍：舊畫面比例不對，先藏起來
                s.frame = None;
                self.texture = None;
                self.size = None;
            }
            if !same_key || s.state != Some(PreviewState::Ready) {
                s.state = Some(PreviewState::Loading { live: fps > 0 });
            }
        }
        let monitor = (!key.is_empty()).then(|| key.to_string());
        let (core, slot, ctx) = (core.clone(), self.slot.clone(), ctx.clone());
        if fps > 0 {
            rt.spawn(async move {
                let Some((mut out, guard, (w, h))) = core.live_preview(fps as f64, monitor.as_deref(), width) else {
                    let mut s = slot.lock().unwrap();
                    s.live_failed = Some(Instant::now());
                    ctx.request_repaint();
                    return;
                };
                let size = [w as usize, h as usize];
                // 直接讀進材質用的格式（不透明的 RGBA 與 Color32 的位元組相同），不用逐像素轉換
                let mut cur = vec![Color32::BLACK; size[0] * size[1]];
                let mut last: Vec<Color32> = Vec::new();
                let mut got = false;
                loop {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if out.read_exact(bytemuck::cast_slice_mut(&mut cur)).await.is_err() {
                        if !got && !stop.load(Ordering::Relaxed) {
                            // 串流失敗（例如 ddagrab 暫時無法使用）：先改用單張
                            slot.lock().unwrap().live_failed = Some(Instant::now());
                        }
                        break;
                    }
                    got = true;
                    // 和上一張一樣（畫面靜止）：不更新材質、不重畫
                    if bytemuck::cast_slice::<Color32, u8>(&cur) == bytemuck::cast_slice::<Color32, u8>(&last) {
                        continue;
                    }
                    std::mem::swap(&mut cur, &mut last);
                    let img = ColorImage::new(size, last.clone());
                    if cur.len() != last.len() {
                        cur = vec![Color32::BLACK; last.len()];
                    }
                    let mut s = slot.lock().unwrap();
                    s.frame = Some(img);
                    s.seq += 1;
                    s.state = Some(PreviewState::Ready);
                    drop(s);
                    ctx.request_repaint();
                }
                drop(guard);
                ctx.request_repaint();
            });
        } else {
            rt.spawn(async move {
                let img = core.preview(monitor.as_deref()).await;
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                let img = img.map(|i| ColorImage::from_rgba_unmultiplied([i.width as usize, i.height as usize], &i.rgba));
                let mut s = slot.lock().unwrap();
                match img {
                    Some(i) => {
                        s.frame = Some(i);
                        s.seq += 1;
                        s.state = Some(PreviewState::Ready);
                    }
                    None => s.state = Some(PreviewState::Failed(tr!("無法取得預覽（仍可錄影）", "Couldn't get a preview (you can still record)").into())),
                }
                drop(s);
                ctx.request_repaint();
            });
        }
    }

    /// 目前的畫面（有新的就更新材質）
    pub fn texture(&mut self, ctx: &egui::Context) -> Option<&TextureHandle> {
        let mut s = self.slot.lock().unwrap();
        if s.live_failed.is_some() && self.current.as_ref().is_some_and(|c| c.fps > 0) {
            // 即時預覽失敗：下一畫格改用單張
            self.current = None;
            self.pending = None;
        }
        if s.seq != self.tex_seq {
            if let Some(img) = s.frame.take() {
                self.size = Some((img.size[0] as u32, img.size[1] as u32));
                match &mut self.texture {
                    Some(t) => t.set(img, TextureOptions::LINEAR),
                    None => self.texture = Some(ctx.load_texture("desk-preview", img, TextureOptions::LINEAR)),
                }
            }
            self.tex_seq = s.seq;
        }
        drop(s);
        self.texture.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_width_follows_display_pixels() {
        // 840 點寬、100%：960 像素一級
        assert_eq!(live_width(840.0, 1.0), 960);
        // 高 DPI（150%）：1260 → 1280
        assert_eq!(live_width(840.0, 1.5), 1280);
        // 很大的視窗也不超過上限
        assert_eq!(live_width(2000.0, 2.0), LIVE_MAX_WIDTH);
        // 很窄時至少 320
        assert_eq!(live_width(50.0, 1.0), 320);
        // 剛好在級距上
        assert_eq!(live_width(640.0, 1.0), 640);
    }

    #[test]
    fn rgba_bytes_are_color32() {
        // 不透明的 RGBA 位元組直接當 Color32 用，和逐像素轉換結果相同
        let rgba = [10u8, 20, 30, 255, 200, 100, 0, 255];
        let mut px = vec![Color32::BLACK; 2];
        bytemuck::cast_slice_mut::<Color32, u8>(&mut px).copy_from_slice(&rgba);
        assert_eq!(px, ColorImage::from_rgba_unmultiplied([2, 1], &rgba).pixels);
    }
}
