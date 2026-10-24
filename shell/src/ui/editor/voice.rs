//! 剪輯視窗的旁白：從播放點開始錄旁白（播放影片、原本的聲音關掉），點擊音效；
//! 播放時一起播旁白（原本的聲音照設定調小）。

use super::Editor;
use crate::ui::theme::{self, segmented, switch, Btn, Icon};
use eframe::egui::{self, vec2, RichText, Sense};
use screenrecorder_core::narration::{self, VoiceClip, VoiceRecorder};
use screenrecorder_core::player::{MediaSpec, Player};
use screenrecorder_core::{tr, trf};
use std::sync::Arc;

pub struct VoiceState {
    mic_id: String,
    /// 錄音中：錄音器、從原影片的哪裡開始
    rec: Option<(VoiceRecorder, f64)>,
    /// 正在播的旁白：檔案、播放器
    play: Option<(String, Player)>,
}

impl VoiceState {
    pub fn new(mic_id: &str) -> VoiceState {
        VoiceState { mic_id: mic_id.to_string(), rec: None, play: None }
    }

    pub fn recording(&self) -> bool {
        self.rec.is_some()
    }
}

/// 每一格：錄音中播到結尾就停；播放中一起播旁白
pub fn tick(ed: &mut Editor) {
    if ed.voice.rec.is_some() {
        if !ed.player.is_playing() {
            stop_recording(ed, None);
        }
        return;
    }
    let t = ed.now();
    let clip = ed.player.is_playing().then(|| ed.spec.narration.clips.iter().find(|c| t >= c.at() && t < c.end() - 0.05).cloned()).flatten();
    match clip {
        Some(c) => {
            if ed.voice.play.as_ref().is_none_or(|(f, _)| *f != c.file) {
                let p = Player::new_audio(ed.ffmpeg.clone(), MediaSpec { path: c.file.clone(), duration: c.ms as f64 / 1000.0, fps: 30.0, has_audio: true }, Arc::new(|| {}));
                p.set_volume((ed.spec.narration.volume as f32 / 100.0).min(1.0));
                p.seek(t - c.at());
                p.play();
                ed.voice.play = Some((c.file.clone(), p));
            }
            ed.set_out_volume(ed.spec.narration.original as f32 / 100.0);
        }
        None => {
            if ed.voice.play.take().is_some() {
                ed.set_out_volume(1.0);
            }
        }
    }
}

fn start_recording(ed: &mut Editor) {
    ed.voice.play = None;
    let at = ed.now();
    // 邊播放邊錄：原本的聲音關掉，免得被麥克風錄進去
    ed.set_out_volume(0.0);
    let rec = VoiceRecorder::start(Arc::new(screenrecorder_core::audio::open_wasapi), &ed.voice.mic_id);
    ed.voice.rec = Some((rec, at));
    ed.player.play();
}

/// 停止錄音並加上這段旁白（和舊的重疊時取代）；toast 收到要顯示的訊息
fn stop_recording(ed: &mut Editor, toast: Option<&mut Option<(String, bool)>>) {
    let Some((rec, at)) = ed.voice.rec.take() else { return };
    ed.player.pause();
    ed.set_out_volume(1.0);
    let path = narration::new_voice_path("voice");
    let msg = match rec.finish(&path) {
        Ok(ms) => {
            let clip = VoiceClip { file: path.to_string_lossy().into_owned(), at_ms: (at * 1000.0).round() as u64, ms, text: String::new() };
            let replaced = ed.spec.narration.clips.iter().filter(|c| c.at_ms < clip.at_ms + clip.ms && c.at_ms + c.ms > clip.at_ms).count();
            ed.spec.narration.put(clip);
            let secs = screenrecorder_core::format::num((ms as f64 / 100.0).round() / 10.0);
            if replaced > 0 {
                (trf!("已錄下 {secs} 秒的旁白，取代了重疊的 {replaced} 段", "Recorded {secs}s of narration, replacing {replaced} overlapping clip(s)"), false)
            } else {
                (trf!("已錄下 {secs} 秒的旁白", "Recorded {secs}s of narration"), false)
            }
        }
        Err(e) => (e, true),
    };
    match toast {
        Some(t) => *t = Some(msg),
        None => ed.voice_msg = Some(msg),
    }
}

/// 「聲音」分頁的旁白與點擊音效
pub fn panel(ed: &mut Editor, ui: &mut egui::Ui, toast: &mut Option<(String, bool)>) {
    let p = theme::pal(ui);
    super::side::section_title(ui, tr!("旁白", "Narration"));
    if let Some((rec, _)) = &ed.voice.rec {
        let (secs, level, err) = (rec.secs(), rec.level(), rec.error());
        ui.horizontal(|ui| {
            ui.label(RichText::new(trf!("● 錄音中 {}", "● Recording {}", screenrecorder_core::format::video_clock(secs))).font(theme::font_bold(13.0)).color(p.rec));
            let (r, _) = ui.allocate_exact_size(vec2((ui.available_width() - 70.0).max(40.0), 8.0), Sense::hover());
            ui.painter().rect_filled(r, 4.0, p.border);
            ui.painter().rect_filled(egui::Rect::from_min_size(r.min, vec2(r.width() * level.clamp(0.0, 1.0), r.height())), 4.0, p.ok);
        });
        if let Some(e) = err {
            ui.label(RichText::new(e).font(theme::font(12.0)).color(p.rec));
        }
        if Btn::new(tr!("停止錄音", "Stop recording")).danger().small().show(ui).clicked() {
            stop_recording(ed, Some(toast));
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        return;
    }
    super::side::hint_text(
        ui,
        tr!(
            "按下後從播放點開始播放影片（不播原本的聲音），邊看邊說；再按一次「停止錄音」。重錄同一段時會取代舊的。",
            "Plays the video from the playhead (without its sound) while you talk; press “Stop recording” when done. Recording over the same part replaces the old clip."
        ),
    );
    if Btn::new(tr!("從播放點開始錄旁白", "Record narration from playhead")).icon(Icon::Mic).small().show(ui).clicked() {
        ed.stop_preview();
        start_recording(ed);
    }
    clip_list(ed, ui);
    if !ed.spec.narration.clips.is_empty() {
        let n = &mut ed.spec.narration;
        ui.horizontal(|ui| {
            ui.label(theme::muted(ui, tr!("旁白音量", "Narration volume")).font(theme::font(12.0)));
            ui.spacing_mut().slider_width = (ui.available_width() - 70.0).max(80.0).floor();
            theme::slider_style(ui);
            ui.add(egui::Slider::new(&mut n.volume, 0..=200).step_by(5.0).suffix("%"));
        });
        ui.label(theme::muted(ui, tr!("有旁白的時候，原本的聲音", "While narration plays, the original audio")).font(theme::font(12.0)));
        let items = [(0u32, tr!("關掉", "Off")), (20, tr!("調小", "Lower")), (100, tr!("不變", "Same"))];
        let mut v = if n.original == 0 || n.original == 100 { n.original } else { 20 };
        if segmented(ui, &mut v, &items, true) {
            n.original = v;
        }
    }

    // 點擊音效
    super::side::section_title_sep(ui, tr!("點擊音效", "Click sounds"));
    if ed.clicks.is_empty() {
        super::side::hint_text(
            ui,
            tr!(
                "這支錄影沒有記下點擊。錄影前在「設定 → 錄影」打開「點擊、按鍵」，錄的時候才會記下點擊。",
                "This recording has no clicks recorded. Turn on “Clicks and keys” in Settings → Recording before you record."
            ),
        );
        return;
    }
    let n = &mut ed.spec.narration;
    let mut on = n.click_volume > 0;
    if switch(ui, &mut on, trf!("每次點擊加上「喀」一聲（{} 次）", "Add a click sound on each click ({})", ed.clicks.len()), true).changed() {
        n.click_volume = if on { 60 } else { 0 };
    }
    if n.click_volume > 0 {
        ui.horizontal(|ui| {
            ui.label(theme::muted(ui, tr!("音量", "Volume")).font(theme::font(12.0)));
            ui.spacing_mut().slider_width = (ui.available_width() - 70.0).max(80.0).floor();
            theme::slider_style(ui);
            ui.add(egui::Slider::new(&mut n.click_volume, 5..=200).step_by(5.0).suffix("%"));
        });
        super::side::hint_text(ui, tr!("輸出的影片才會加上。", "Added to the output video only."));
    }
}

/// 旁白清單：時間、內容、試聽、刪除
fn clip_list(ed: &mut Editor, ui: &mut egui::Ui) {
    let p = theme::pal(ui);
    let mut remove = None;
    let mut seek = None;
    for (i, c) in ed.spec.narration.clips.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let missing = !std::path::Path::new(&c.file).is_file();
            let label = narration::describe(c);
            // 文字太長時截短，右邊留給「播放」「×」
            let w = (ui.available_width() - 84.0).max(60.0);
            let r = ui
                .allocate_ui_with_layout(vec2(w, 22.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.set_width(w);
                    ui.add(egui::Label::new(RichText::new(label).font(theme::font(12.5)).color(if missing { p.rec } else { p.text })).truncate())
                })
                .inner;
            if missing {
                r.on_hover_text(tr!("找不到這段旁白的檔案，輸出時會略過", "This narration file is missing and will be skipped"));
            } else if !c.text.is_empty() {
                r.on_hover_text(&c.text);
            }
            if Btn::new(tr!("播放", "Play")).ghost().small().tooltip(tr!("從這段旁白開始播放", "Play from this clip")).show(ui).clicked() {
                seek = Some(c.at());
            }
            if Btn::new("×").ghost().small().tooltip(tr!("刪除這段旁白", "Delete this clip")).show(ui).clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        ed.spec.narration.clips.remove(i);
        ed.voice.play = None;
        ed.set_out_volume(1.0);
    }
    if let Some(t) = seek {
        ed.stop_preview();
        ed.seek(t);
        ed.player.play();
    }
}
