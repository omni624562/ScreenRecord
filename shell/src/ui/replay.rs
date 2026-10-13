//! 重播緩衝（操作視窗）：錄影面板右上角「重播」選要保留多久、開始；重播緩衝中「存下最近 N 分鐘」。
//! 錄影與存檔都在 core（recorder.rs、replay.rs）；快速鍵與系統匣也能開始、存下。

use super::theme::{self, Btn, Icon};
use super::UiApp;
use eframe::egui::{self, RichText, Ui};
use screenrecorder_core::replay::{length_label, LENGTHS};
use screenrecorder_core::{tr, trf};

/// 右上角的按鈕（沒在錄影時）：說明、選長度、開始
pub fn button(app: &mut UiApp, ui: &mut Ui) {
    let can = app.env.ffmpeg.found && app.env.ffmpeg.encoder.is_some() && !app.exporting();
    let tip = tr!(
        "重播緩衝：在背景一直錄、只保留最近幾分鐘，發生想留下的事時再存下來",
        "Replay buffer: keeps recording in the background, holding only the last few minutes; save them when something worth keeping happens"
    );
    let resp = Btn::new(tr!("重播", "Replay")).icon(Icon::Rewind).ghost().small().enabled(can).tooltip(tip).show(ui);
    let mut start = false;
    egui::Popup::menu(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        let p = theme::pal(ui);
        ui.set_width(340.0);
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.label(RichText::new(tr!("重播緩衝", "Replay buffer")).font(theme::font_bold(14.0)));
        ui.add(
            egui::Label::new(
                RichText::new(tr!(
                    "在背景一直錄，只保留最近幾分鐘；發生想留下的事（精彩畫面、程式突然出錯）時再存下來。用目前的範圍與聲音設定，不倒數、不縮小視窗。",
                    "Keeps recording in the background, holding only the last few minutes. When something worth keeping happens (a great moment, an unexpected error), save it. Uses the current area and audio settings, with no countdown and without minimizing this window."
                ))
                .font(theme::font(12.5))
                .color(p.muted),
            )
            .wrap(),
        );
        ui.label(RichText::new(tr!("保留最近", "Keep the last")).font(theme::font(13.0)));
        let labels: Vec<(u32, String)> = LENGTHS.iter().map(|s| (*s, length_label(*s))).collect();
        let items: Vec<(u32, &str)> = labels.iter().map(|(s, l)| (*s, l.as_str())).collect();
        let mut sec = app.s.replay_sec;
        if theme::segmented(ui, &mut sec, &items, true) {
            app.s.replay_sec = sec;
            app.save_settings();
        }
        ui.add_space(2.0);
        if Btn::new(tr!("開始重播緩衝", "Start replay buffer")).icon(Icon::Play).primary().show(ui).clicked() {
            start = true;
        }
        let key = app.keys.label(6);
        if !key.is_empty() {
            ui.label(theme::muted(ui, trf!("快速鍵 {key}：開始，重播緩衝中再按就存下", "Shortcut {key}: start; press again while running to save")));
        }
    });
    if start {
        let config = screenrecorder_core::types::RecordConfig { replay_sec: app.s.replay_sec, ..app.s.record_config(&app.env) };
        let core = app.core.clone();
        app.guarded(async move { core.replay_start(config).await }, |_, _| {});
    }
}

/// 存下最近 N 分鐘（錄影面板的按鈕）
pub fn save(app: &mut UiApp) {
    let core = app.core.clone();
    app.spawn(async move { core.replay_save(false).await }, |app, r| match r {
        Ok(res) => {
            let name = res.path.as_deref().and_then(|p| std::path::Path::new(p).file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            app.toast(trf!("已存下 {name}（{}）", "Saved {name} ({})", screenrecorder_core::format::video_clock(res.video_sec)), false);
            app.recent_page = 1;
            app.load_recent();
        }
        Err(e) => app.toast(e.message().to_string(), true),
    });
}
