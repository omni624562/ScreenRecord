//! 檢視器與主畫面內播放的影片：有 libmpv 時用 mpv（畫面直接畫進 OpenGL 材質，跟 Windows 的播放器一樣順），
//! 沒有或 mpv 播不了時用 FFmpeg 解碼（core::player）。

use eframe::egui;
use eframe::glow::{self, HasContext};
use screenrecorder_core::mpv::MpvPlayer;
use screenrecorder_core::player::Player;
use std::sync::Arc;

pub enum Playback {
    Ff(Player),
    Mpv { p: MpvPlayer, has_audio: bool },
}

impl Playback {
    pub fn time(&self) -> f64 {
        match self {
            Playback::Ff(p) => p.time(),
            Playback::Mpv { p, .. } => p.time(),
        }
    }

    pub fn is_playing(&self) -> bool {
        match self {
            Playback::Ff(p) => p.is_playing(),
            Playback::Mpv { p, .. } => p.is_playing(),
        }
    }

    pub fn play(&self) {
        match self {
            Playback::Ff(p) => p.play(),
            Playback::Mpv { p, .. } => p.play(),
        }
    }

    pub fn pause(&self) {
        match self {
            Playback::Ff(p) => p.pause(),
            Playback::Mpv { p, .. } => p.pause(),
        }
    }

    pub fn seek(&self, t: f64) {
        match self {
            Playback::Ff(p) => p.seek(t),
            Playback::Mpv { p, .. } => p.seek(t),
        }
    }

    pub fn rate(&self) -> f64 {
        match self {
            Playback::Ff(p) => p.rate(),
            Playback::Mpv { p, .. } => p.rate(),
        }
    }

    pub fn set_rate(&self, r: f64) {
        match self {
            Playback::Ff(p) => p.set_rate(r),
            Playback::Mpv { p, .. } => p.set_rate(r),
        }
    }

    pub fn set_volume(&self, v: f32) {
        match self {
            Playback::Ff(p) => p.set_volume(v),
            Playback::Mpv { p, .. } => p.set_volume(v),
        }
    }

    pub fn take_ended(&self) -> bool {
        match self {
            Playback::Ff(p) => p.take_ended(),
            Playback::Mpv { p, .. } => p.take_ended(),
        }
    }

    pub fn duration(&self) -> f64 {
        match self {
            Playback::Ff(p) => p.spec().duration,
            Playback::Mpv { p, .. } => p.duration(),
        }
    }

    pub fn has_audio(&self) -> bool {
        match self {
            Playback::Ff(p) => p.spec().has_audio,
            Playback::Mpv { has_audio, .. } => *has_audio,
        }
    }
}

/// 每一格開始時（OpenGL context 是目前的）：處理 mpv 的事件，有新畫面就畫到 surface（第一次用時建立）。
/// size：要畫多大（像素）；回傳畫好的材質（還沒有畫面時 None）。mpv 播不了時回傳錯誤，呼叫的地方改用 FFmpeg
pub fn render_mpv(gl: Option<&Arc<glow::Context>>, surface: &mut Option<VideoSurface>, frame: &mut eframe::Frame, p: &mut MpvPlayer, size: [u32; 2]) -> Result<Option<egui::TextureId>, String> {
    p.poll();
    if let Some(e) = p.failed() {
        return Err(e.to_string());
    }
    let Some(gl) = gl else { return Err("沒有 OpenGL".into()) };
    if surface.is_none() {
        *surface = Some(VideoSurface::new(gl, frame)?);
    }
    let s = surface.as_mut().expect("剛建立");
    Ok(s.draw(p, size)?.then_some(s.id))
}

/// mpv 畫在材質上是上下顛倒的（不顛倒時直拍的影片在某些 mpv 版本畫不出來）：顯示時用這個 uv 翻回來
pub fn mpv_uv() -> egui::Rect {
    egui::Rect::from_min_max(egui::pos2(0.0, 1.0), egui::pos2(1.0, 0.0))
}

/// OpenGL 是軟體算的嗎（沒有顯示卡的虛擬機器：Mesa llvmpipe / softpipe、Windows 的 GDI Generic）
pub fn software_gl(gl: &glow::Context) -> bool {
    // SAFETY: 介面執行緒，OpenGL context 是目前的
    let r = unsafe { gl.get_parameter_string(glow::RENDERER) }.to_lowercase();
    ["llvmpipe", "softpipe", "software", "gdi generic"].iter().any(|k| r.contains(k))
}

/// mpv 畫畫面的 OpenGL 材質（整個程式共用一張，大小跟著顯示的大小）：註冊成 egui 的材質，照一般圖片顯示
pub struct VideoSurface {
    gl: Arc<glow::Context>,
    tex: glow::Texture,
    fbo: glow::Framebuffer,
    size: [u32; 2],
    pub id: egui::TextureId,
}

impl VideoSurface {
    pub fn new(gl: &Arc<glow::Context>, frame: &mut eframe::Frame) -> Result<VideoSurface, String> {
        // SAFETY: 介面執行緒，OpenGL context 是目前的
        unsafe {
            let tex = gl.create_texture()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
            gl.bind_texture(glow::TEXTURE_2D, None);
            let fbo = gl.create_framebuffer()?;
            let mut s = VideoSurface { gl: gl.clone(), tex, fbo, size: [0, 0], id: frame.register_native_glow_texture(tex) };
            s.resize([16, 16])?;
            Ok(s)
        }
    }

    fn resize(&mut self, size: [u32; 2]) -> Result<(), String> {
        let gl = &self.gl;
        // SAFETY: 同上；同一張材質重新配置大小（egui 註冊的 id 不變）
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(self.tex));
            gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8 as i32, size[0] as i32, size[1] as i32, 0, glow::RGBA, glow::UNSIGNED_BYTE, glow::PixelUnpackData::Slice(None));
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.fbo));
            gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(self.tex), 0);
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            if status != glow::FRAMEBUFFER_COMPLETE {
                return Err(format!("FBO 不完整：0x{status:x}"));
            }
        }
        self.size = size;
        Ok(())
    }

    /// 有新畫面（或大小變了）就請 mpv 畫上去；回傳有沒有畫
    pub fn draw(&mut self, p: &MpvPlayer, size: [u32; 2]) -> Result<bool, String> {
        let size = [size[0].clamp(16, 8192), size[1].clamp(16, 8192)];
        let resized = size != self.size;
        if resized {
            self.resize(size)?;
        }
        if !p.wants_render() && !resized {
            return Ok(false);
        }
        reset_state(&self.gl);
        p.render(self.fbo.0.get(), size[0], size[1], true);
        // SAFETY: 同上；mpv 畫完把 framebuffer 換回畫面
        unsafe { self.gl.bind_framebuffer(glow::FRAMEBUFFER, None) };
        Ok(true)
    }
}

/// mpv 要 OpenGL 在預設狀態（見 mpv/render_gl.h）：egui 畫完上一格留下的設定先還原（不然只畫到一小條或全黑）。
/// egui 下一次畫的時候會再設定自己要的
fn reset_state(gl: &glow::Context) {
    // SAFETY: 介面執行緒，OpenGL context 是目前的
    unsafe {
        gl.disable(glow::SCISSOR_TEST);
        gl.disable(glow::BLEND);
        gl.disable(glow::FRAMEBUFFER_SRGB);
        gl.color_mask(true, true, true, true);
        gl.use_program(None);
        gl.bind_vertex_array(None);
        gl.bind_buffer(glow::ARRAY_BUFFER, None);
        gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, None);
        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, None);
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
        gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, 0);
        gl.pixel_store_i32(glow::PACK_ALIGNMENT, 4);
    }
}

impl Drop for VideoSurface {
    fn drop(&mut self) {
        // SAFETY: 同上
        unsafe {
            self.gl.delete_framebuffer(self.fbo);
            self.gl.delete_texture(self.tex);
        }
    }
}
