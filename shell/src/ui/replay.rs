//! 回溯錄影（操作視窗）：錄影面板右上角「回溯錄影」選要保留多久、開始；回溯錄影中「存下最近 N 分鐘」，停止前先問要不要存。
//! 錄影與存檔都在 core（recorder.rs、replay.rs）；快速鍵與系統匣也能開始、存下。

use super::theme::{self, Btn, Icon};
use super::UiApp;
use eframe::egui::{self, RichText, Ui};
use screenrecorder_core::replay::{length_label, LENGTHS};
use screenrecorder_core::{tr, trf};

/// 回溯錄影的說明、保留長度、開始按鈕（主畫面「更多工具」的子選單也用這個）
pub fn menu_body(app: &mut UiApp, ui: &mut Ui) {
    let mut start = false;
    {
        let p = theme::pal(ui);
        ui.set_width(340.0);
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.label(RichText::new(tr!("回溯錄影：在背景一直錄，只留最近幾分鐘", "Replay buffer: keeps recording in the background, holding only the last few minutes")).font(theme::font_bold(14.0)));
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
        if Btn::new(tr!("開始回溯錄影", "Start replay buffer")).icon(Icon::Play).primary().show(ui).clicked() {
            start = true;
        }
        let key = app.keys.label(6);
        if !key.is_empty() {
            ui.label(theme::muted(ui, trf!("快速鍵 {key}：開始，回溯錄影中再按就存下", "Shortcut {key}: start; press again while running to save")));
        }
    }
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

/// 停止回溯錄影：還有沒存下的內容時先問（先存下再停止 / 不存，直接停止 / 取消）
pub fn ask_stop(app: &mut UiApp, rp: &screenrecorder_core::types::ReplayStatus, recorded_ms: f64) {
    let stop = |app: &mut UiApp| {
        let core = app.core.clone();
        app.guarded(async move { core.recorder.stop(None).await }, |_, _| {});
    };
    // 緩衝裡幾乎沒有東西（剛開始、剛存過）：直接停止
    let held = (recorded_ms / 1000.0).min(rp.sec as f64);
    if held < 3.0 || rp.bytes == 0 {
        return stop(app);
    }
    let len = if held + 1.0 >= rp.sec as f64 { length_label(rp.sec) } else { screenrecorder_core::format::human_duration(held.floor()) };
    let mut ask = super::dialogs::Ask::confirm(
        tr!("停止回溯錄影？", "Stop the replay buffer?"),
        trf!("最近 {len}還沒存下，停止後會刪掉。", "The last {len} hasn't been saved and will be deleted when you stop."),
        tr!("先存下再停止", "Save, then stop"),
        move |app, _| {
            app.ask = None;
            let core = app.core.clone();
            app.spawn(
                async move {
                    let r = core.replay_save(false).await;
                    let _ = core.recorder.stop(None).await;
                    r
                },
                |app, r| match r {
                    Ok(res) => {
                        let name = res.path.as_deref().and_then(|p| std::path::Path::new(p).file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        app.toast(trf!("已存下 {name}，回溯錄影已停止", "Saved {name}; the replay buffer has stopped"), false);
                        app.recent_page = 1;
                        app.load_recent();
                    }
                    Err(e) => app.toast(e.message().to_string(), true),
                },
            );
        },
    )
    .with_alt(tr!("不存，直接停止", "Don't save, just stop"), move |app, _| stop(app));
    ask.alt_danger = true;
    app.ask = Some(ask);
}
