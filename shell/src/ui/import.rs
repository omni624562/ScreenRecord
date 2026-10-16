//! 匯入影片：把影片拖進主視窗或「全部錄影」，或按「匯入影片…」選檔案。
//! MP4 複製到儲存位置；MOV、MKV 等其他格式轉成 MP4 存到儲存位置（原檔都不動）。見 core::actions::import_videos。

use super::theme;
use super::UiApp;
use eframe::egui::{self, Color32, Id};
use screenrecorder_core::{actions, tr, trf};

/// 檔案選擇視窗的類型
const FILTER: &str = "*.mp4;*.mov;*.mkv;*.avi;*.wmv;*.webm;*.m4v;*.flv;*.mpg;*.mpeg;*.ts;*.3gp";

/// 主畫面、全部錄影：拖曳檔案到視窗時蓋上提示，放開就匯入（剪輯視窗開著時由剪輯視窗處理：拖進來的是圖片）
pub fn handle_drop(app: &mut UiApp, ctx: &egui::Context) {
    if app.editor.is_some() {
        return;
    }
    let (hovering, dropped) = ctx.input(|i| (!i.raw.hovered_files.is_empty(), i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect::<Vec<_>>()));
    if hovering {
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::new("drop-import")));
        let r = ctx.content_rect();
        painter.rect_filled(r, 0, Color32::from_black_alpha(150));
        painter.text(r.center() - egui::vec2(0.0, 14.0), egui::Align2::CENTER_CENTER, tr!("放開滑鼠匯入影片", "Release to import videos"), theme::font_bold(22.0), Color32::WHITE);
        painter.text(
            r.center() + egui::vec2(0.0, 18.0),
            egui::Align2::CENTER_CENTER,
            tr!("MOV、MKV 等會自動轉成 MP4，存到儲存位置；原本的檔案不會動", "MOV, MKV and other formats are converted to MP4 in your save folder. The original files stay as they are"),
            theme::font(14.0),
            Color32::from_gray(225),
        );
    }
    if !dropped.is_empty() {
        let paths: Vec<String> = dropped.iter().map(|p| p.display().to_string()).collect();
        // 從清單拖出去又放回這個視窗：不是要匯入
        let own = app.dragged_out.as_ref().is_some_and(|(list, at)| at.elapsed().as_secs() < 5 && paths.iter().all(|p| list.iter().any(|l| l.eq_ignore_ascii_case(p))));
        if !own {
            start(app, paths);
        }
    }
}

/// 「匯入影片…」：選一或多個影片
pub fn pick(app: &mut UiApp) {
    app.spawn(
        async move {
            tokio::task::spawn_blocking(|| screenrecorder_core::filepick::pick_many(tr!("匯入影片", "Import videos"), &[(tr!("影片", "Videos"), FILTER)])).await.unwrap_or_else(|e| Err(e.to_string()))
        },
        |app, r| match r {
            Ok(list) if !list.is_empty() => start(app, list.iter().map(|p| p.display().to_string()).collect()),
            Ok(_) => {}
            Err(e) => app.toast(e, true),
        },
    );
}

/// 開始匯入（一次一批；錄影中不能匯入）
pub fn start(app: &mut UiApp, paths: Vec<String>) {
    let videos: Vec<String> = paths.iter().filter(|p| screenrecorder_core::import::is_importable(p)).cloned().collect();
    if videos.is_empty() {
        return app.toast(tr!("只能匯入影片（MP4、MOV、MKV、AVI、WMV、WebM…）", "Only videos can be imported (MP4, MOV, MKV, AVI, WMV, WebM…)"), true);
    }
    if app.importing {
        return app.toast(tr!("上一批影片還在匯入，完成後再拖進來", "The previous videos are still being imported. Try again when they're done"), true);
    }
    if app.core.recorder.active() {
        return app.toast(tr!("錄影中不能匯入影片，請先停止錄影", "Videos can't be imported while recording. Stop recording first"), true);
    }
    if let Some(why) = app.blocked(super::EntryAction::Export) {
        return app.toast(why, true);
    }
    app.importing = true;
    let skipped = paths.len() - videos.len();
    let n = videos.len();
    app.toast(
        if skipped > 0 {
            trf!("正在匯入 {n} 支影片（略過 {skipped} 個不是影片的檔案）…", "Importing {n} video(s) ({skipped} non-video file(s) skipped)…")
        } else {
            trf!("正在匯入 {n} 支影片…", "Importing {n} video(s)…")
        },
        false,
    );
    let core = app.core.clone();
    let dest = std::path::PathBuf::from(app.s.out_dir(&app.env));
    app.spawn(async move { actions::import_videos(&core, &videos, &dest).await }, move |app, r| {
        app.importing = false;
        app.load_recent();
        app.shots_changed();
        let done = r.done.len();
        let mut msg = match (done, r.already) {
            (0, 0) => String::new(),
            (d, 0) => trf!("已匯入 {d} 支影片到儲存位置", "Imported {d} video(s) to your save folder"),
            (0, a) => trf!("{a} 支影片本來就在儲存位置裡", "{a} video(s) were already in your save folder"),
            (d, a) => trf!("已匯入 {d} 支影片（另外 {a} 支本來就在儲存位置裡）", "Imported {d} video(s) ({a} already in your save folder)"),
        };
        if !r.failed.is_empty() {
            let names: Vec<&str> = r.failed.iter().map(|f| f.0.as_str()).take(3).collect();
            let why = r.failed.first().map(|f| f.1.clone()).filter(|w| !w.is_empty()).map(|w| trf!("：{w}", ": {w}")).unwrap_or_default();
            msg = format!(
                "{msg}{}{}",
                if msg.is_empty() { "" } else { tr!("；", "; ") },
                trf!("{} 支無法匯入（{}）{why}", "{} couldn't be imported ({}){why}", r.failed.len(), names.join(tr!("、", ", ")))
            );
        }
        if r.canceled {
            msg = format!("{msg}{}{}", if msg.is_empty() { "" } else { tr!("；", "; ") }, tr!("開始錄影，剩下的停止匯入", "Recording started, so the rest were not imported"));
        }
        let warn = !r.failed.is_empty() || r.canceled || done == 0;
        match r.done.last() {
            Some(last) if !warn => app.toast_file(msg, last),
            _ => app.toast(msg, warn),
        }
    });
}
