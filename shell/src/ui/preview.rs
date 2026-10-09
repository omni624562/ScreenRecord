//! 擷取範圍的預覽：即時模式由 FFmpeg 持續輸出 RGBA 畫面（平常每秒 5 張、錄影中 2 張），
//! 否則（或即時預覽失敗時）只擷取一張。視窗隱藏時停止。

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use screenrecorder_core::app::App;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::AsyncReadExt;

#[derive(Debug, Clone, PartialEq)]
pub enum PreviewState {
    Idle,
    Loading { live: bool },
    Ready,
    Failed(String),
}

#[derive(Default)]
struct Slot {
    frame: Option<(u32, u32, Vec<u8>)>,
    seq: u64,
    state: Option<PreviewState>,
    /// 即時預覽失敗：改用單張
    live_failed: bool,
}

pub struct Preview {
    /// 目前顯示的範圍與張數；None = 需要重新開始
    current: Option<(String, u32)>,
    slot: Arc<Mutex<Slot>>,
    stop: Arc<AtomicBool>,
    texture: Option<TextureHandle>,
    tex_seq: u64,
    pub size: Option<(u32, u32)>,
}

impl Default for Preview {
    fn default() -> Self {
        Preview { current: None, slot: Arc::default(), stop: Arc::new(AtomicBool::new(false)), texture: None, tex_seq: 0, size: None }
    }
}

impl Preview {
    /// 下次畫面時重新開始（換了範圍、重新偵測、開始 / 結束錄影）
    pub fn reset(&mut self) {
        self.current = None;
    }

    /// 重新嘗試即時預覽
    pub fn retry_live(&mut self) {
        self.slot.lock().unwrap().live_failed = false;
        self.current = None;
    }

    pub fn stop(&mut self, core: &App) {
        self.stop.store(true, Ordering::Relaxed);
        core.stop_live();
        self.current = None;
    }

    /// 預覽在執行中（或已要求執行）
    pub fn running(&self) -> bool {
        self.current.is_some()
    }

    pub fn state(&self) -> PreviewState {
        self.slot.lock().unwrap().state.clone().unwrap_or(PreviewState::Idle)
    }

    /// 確認預覽在執行（key = "" 整個桌面或螢幕 id；fps = 0 單張）
    pub fn ensure(&mut self, core: &Arc<App>, rt: &tokio::runtime::Handle, ctx: &egui::Context, key: &str, mut fps: u32, ffmpeg_ok: bool) {
        if !ffmpeg_ok {
            self.slot.lock().unwrap().state = Some(PreviewState::Failed("需要 FFmpeg 才能顯示預覽".into()));
            return;
        }
        if self.slot.lock().unwrap().live_failed {
            fps = 0;
        }
        let want = (key.to_string(), fps);
        if self.current.as_ref() == Some(&want) {
            return;
        }
        let key_changed = self.current.as_ref().map(|c| c.0 != want.0).unwrap_or(true);
        self.current = Some(want);
        self.stop.store(true, Ordering::Relaxed);
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = stop.clone();
        {
            let mut s = self.slot.lock().unwrap();
            if key_changed {
                // 換了範圍：舊畫面比例不對，先藏起來
                s.frame = None;
                self.texture = None;
                self.size = None;
            }
            if key_changed || s.state != Some(PreviewState::Ready) {
                s.state = Some(PreviewState::Loading { live: fps > 0 });
            }
        }
        let monitor = (!key.is_empty()).then(|| key.to_string());
        let (core, slot, ctx) = (core.clone(), self.slot.clone(), ctx.clone());
        if fps > 0 {
            rt.spawn(async move {
                let Some((mut out, guard, (w, h))) = core.live_preview(fps as f64, monitor.as_deref()) else {
                    let mut s = slot.lock().unwrap();
                    s.live_failed = true;
                    ctx.request_repaint();
                    return;
                };
                let mut buf = vec![0u8; (w * h * 4) as usize];
                let mut got = false;
                loop {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if out.read_exact(&mut buf).await.is_err() {
                        if !got && !stop.load(Ordering::Relaxed) {
                            // 串流失敗（例如 ddagrab 暫時無法使用）：這次改用單張
                            slot.lock().unwrap().live_failed = true;
                        }
                        break;
                    }
                    got = true;
                    let mut s = slot.lock().unwrap();
                    s.frame = Some((w, h, buf.clone()));
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
                let mut s = slot.lock().unwrap();
                match img {
                    Some(i) => {
                        s.frame = Some((i.width, i.height, i.rgba));
                        s.seq += 1;
                        s.state = Some(PreviewState::Ready);
                    }
                    None => s.state = Some(PreviewState::Failed("無法取得預覽（仍可錄影）".into())),
                }
                drop(s);
                ctx.request_repaint();
            });
        }
    }

    /// 目前的畫面（有新的就更新材質）
    pub fn texture(&mut self, ctx: &egui::Context) -> Option<&TextureHandle> {
        let mut s = self.slot.lock().unwrap();
        if s.live_failed && self.current.as_ref().is_some_and(|c| c.1 > 0) {
            // 即時預覽失敗：下一畫格改用單張
            self.current = None;
        }
        if s.seq != self.tex_seq {
            if let Some((w, h, rgba)) = s.frame.take() {
                let img = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                self.size = Some((w, h));
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
