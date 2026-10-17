//! 一次處理多支錄影：全部錄影勾選幾支後，一次剪掉沒動靜的片段、產生字幕、做加速版、壓縮（core::after_record 一支一支做）。

use super::super::theme::{self, Btn};
use super::super::UiApp;
use super::LibraryDialog;
use eframe::egui::{self, Align, Id, Layout, RichText};
use screenrecorder_core::after_record::AfterRecord;
use screenrecorder_core::edit::FAST_SPEEDS;
use screenrecorder_core::subtitles::language_name;
use screenrecorder_core::{tr, trf};

pub struct BatchDlg {
    paths: Vec<String>,
    idle_cut: bool,
    subs: bool,
    subs_en: bool,
    /// 加速版幾倍（0 = 不做）
    speed: u32,
    /// 壓縮到幾 MB 以內（0 = 不壓縮）
    mb: u32,
    /// 這次的選擇也存成「錄完自動處理」的設定（兩邊共用同一套）
    save: bool,
}

impl BatchDlg {
    /// 預設沿用「錄完自動處理」的設定
    pub fn new(app: &UiApp, paths: Vec<String>) -> BatchDlg {
        let s = &app.s;
        BatchDlg { paths, idle_cut: s.after_idle_cut, subs: s.after_subs, subs_en: s.after_subs_en, speed: s.after_speed, mb: s.after_compress_mb, save: false }
    }

    fn any(&self) -> bool {
        self.idle_cut || self.subs || self.speed > 0 || self.mb > 0
    }
}

/// 每一列的高度（有沒有右邊的選項都一樣高）
const ROW_H: f32 = 32.0;

fn row(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), ROW_H), Layout::left_to_right(Align::Center), add);
}

/// 一次處理多支正在進行：第幾支、總共幾支
pub fn running(app: &UiApp) -> Option<(usize, usize)> {
    app.core.after_status().and_then(|s| s.batch)
}

pub fn show(app: &mut UiApp, ctx: &egui::Context, d: &mut LibraryDialog) {
    let Some(b) = &mut d.batch else { return };
    let (mut close, mut start) = (false, false);
    let modal = egui::Modal::new(Id::new("lib-batch")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        ui.set_width(460.0);
        let p = theme::pal(ui);
        let n = b.paths.len();
        ui.label(RichText::new(trf!("一次處理 {n} 支錄影", "Process {n} recordings")).font(theme::font_bold(16.0)));
        ui.add_space(2.0);
        ui.add(
            egui::Label::new(theme::muted(
                ui,
                tr!(
                    "預設和「錄完自動處理」一樣。勾選要做的事，會一支一支依序處理；原本的錄影都會保留，做好的另存在同一個資料夾。",
                    "Starts with your After recording settings. Choose what to do; recordings are processed one at a time, the originals are kept and the results are saved in the same folder."
                ),
            ))
            .wrap(),
        );
        ui.add_space(10.0);
        row(ui, |ui| {
            theme::switch(ui, &mut b.idle_cut, tr!("剪掉沒動靜的片段", "Cut idle parts"), true)
                .on_hover_text(tr!("畫面不動而且沒有聲音超過 2 秒的地方剪掉，另存剪輯版", "Removes parts where nothing moves and there's no sound for over 2 seconds, saved as an edited version"));
        });
        row(ui, |ui| {
            theme::switch(ui, &mut b.subs, trf!("產生字幕（{}）", "Generate subtitles ({})", language_name(app.s.after_subs_lang as usize)), true)
                .on_hover_text(tr!("語言與辨識模型沿用「設定 → 錄完自動處理」", "Language and model follow Settings → After recording"));
        });
        if b.subs {
            row(ui, |ui| {
                ui.add_space(28.0);
                theme::check(ui, &mut b.subs_en, tr!("也翻成英文（.en.srt）", "Also translate to English (.en.srt)"));
            });
        }
        row(ui, |ui| {
            let mut on = b.speed > 0;
            if theme::switch(ui, &mut on, tr!("製作加速版", "Make a sped-up version"), true).changed() {
                b.speed = if on { 4 } else { 0 };
            }
            if b.speed > 0 {
                let items: Vec<(u32, String)> = FAST_SPEEDS.iter().map(|v| (*v, format!("{v}×"))).collect();
                let items: Vec<(u32, &str)> = items.iter().map(|(v, s)| (*v, s.as_str())).collect();
                theme::segmented(ui, &mut b.speed, &items, true);
            }
        });
        row(ui, |ui| {
            let mut on = b.mb > 0;
            if theme::switch(ui, &mut on, tr!("壓縮", "Compress"), true).changed() {
                b.mb = if on { 25 } else { 0 };
            }
            if b.mb > 0 {
                let items: Vec<(u32, String)> = super::super::settings::MAX_MB.iter().filter(|v| **v > 0).map(|v| (*v, trf!("{v} MB 以內", "Under {v} MB"))).collect();
                let items: Vec<(u32, &str)> = items.iter().map(|(v, s)| (*v, s.as_str())).collect();
                theme::segmented(ui, &mut b.mb, &items, true);
            }
        });
        ui.add_space(6.0);
        theme::check(ui, &mut b.save, RichText::new(tr!("也存成「錄完自動處理」的設定", "Also use these for After recording")).font(theme::font(12.5)))
            .on_hover_text(tr!("之後每次錄完也照這些設定處理（設定 → 錄完處理）", "Every new recording is then processed the same way (Settings → After recording)"));
        ui.add_space(10.0);
        ui.label(
            RichText::new(tr!(
                "處理時可以繼續做別的事；開始錄影時會先暫停，錄完再繼續。進度顯示在主畫面的錄影面板，也可以在那裡取消。",
                "You can keep working meanwhile; it pauses while you record and continues afterwards. Progress shows in the recording panel on the main screen, where you can also cancel."
            ))
            .font(theme::font(12.0))
            .color(p.muted),
        );
        ui.add_space(12.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let tip = if b.any() { "" } else { tr!("至少選一項", "Choose at least one") };
            if Btn::new(tr!("開始處理", "Start")).primary().enabled(b.any()).tooltip(tip).show(ui).clicked() {
                start = true;
            }
            if Btn::new(tr!("取消", "Cancel")).ghost().show(ui).clicked() {
                close = true;
            }
        });
    });
    if modal.should_close() || close {
        d.batch = None;
        return;
    }
    if !start {
        return;
    }
    let Some(b) = d.batch.take() else { return };
    if b.save {
        (app.s.after_idle_cut, app.s.after_subs, app.s.after_subs_en, app.s.after_speed, app.s.after_compress_mb) = (b.idle_cut, b.subs, b.subs_en, b.speed, b.mb);
        app.save_settings();
    }
    let after = AfterRecord {
        idle_cut: b.idle_cut,
        subtitles: b.subs,
        subs_model: app.s.after_subs_model as usize,
        subs_lang: app.s.after_subs_lang as usize,
        subs_en: b.subs && b.subs_en,
        compress_mb: b.mb,
        speed: b.speed as f64,
        keep_audio: app.s.keep_audio,
        auto_name: false,
        // 一次處理多支時不複製（要複製的話在檔案總管一次複製比較快）
        copy_to: String::new(),
        jellyfin: None,
    };
    let n = b.paths.len();
    let core = app.core.clone();
    app.rt.spawn(core.batch_process(b.paths, after));
    d.selected.clear();
    app.dismissed_job = None;
    app.toast(trf!("開始處理 {n} 支錄影，進度顯示在主畫面的錄影面板", "Processing {n} recordings. Progress shows in the recording panel on the main screen"), false);
}
