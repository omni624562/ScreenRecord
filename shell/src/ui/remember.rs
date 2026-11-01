//! 記住上次的操作選擇：檔案庫的排序與篩選、剪輯視窗的分頁與工具、播放速度、設定停在哪一頁……
//! 各畫面開啟時讀設定的 last（settings::LastChoices）；這裡每一格把目前的選擇記回去，有改才存檔（延後一下一起存）。

use super::UiApp;

/// 讀了設定之後：還原只放在記憶體裡的（右側面板、標註的顏色大小、播放速度、Jellyfin 字幕開關）
pub fn restore(app: &UiApp) {
    super::editor::restore_last(&app.s.last);
    super::viewer::restore_last(&app.s.last);
}

/// 每一格：把目前的選擇記到設定，有改才存
pub fn sync(app: &mut UiApp) {
    let mut last = app.s.last.clone();
    super::editor::remember_statics(&mut last);
    super::viewer::remember(&mut last);
    if let Some(d) = &app.library {
        d.remember(&mut last);
    }
    if let Some(ed) = &app.editor {
        ed.remember(&mut last);
    }
    if let Some(d) = &app.settings_dlg {
        super::settings_dialog::remember(d, &mut last);
    }
    if last != app.s.last {
        app.s.last = last;
        app.save_settings();
    }
}
