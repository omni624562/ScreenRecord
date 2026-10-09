//! 操作視窗呼叫的動作（原本的本機 HTTP API）：檢查輸入後交給錄影器、轉檔、清單、剪輯專案。
//! 錯誤訊息直接顯示給使用者。

use crate::app::App;
use crate::edit::EditSpec;
use crate::error::{Error, Result};
use crate::projects::Project;
use crate::types::{ExportFormat, ExportState, LibraryEntry, LibraryPage, LibraryQuery, RecordConfig, UpdateInfo};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn is_file(p: &str) -> bool {
    std::fs::metadata(p).map(|m| m.is_file()).unwrap_or(false)
}

fn ends_with_ci(p: &str, exts: &[&str]) -> bool {
    let l = p.to_lowercase();
    exts.iter().any(|e| l.ends_with(e))
}

/// 只接受完整路徑（不讓相對路徑落到程式所在的資料夾）
fn is_abs(p: &str) -> bool {
    Path::new(p).is_absolute() || crate::paths::is_windows_abs(p)
}

/// 正在轉檔的來源與輸出（小寫）
fn exporting(app: &App) -> Vec<String> {
    match app.exporter.status() {
        Some(j) if j.state == ExportState::Running => vec![j.source.to_lowercase(), j.output.to_lowercase()],
        _ => vec![],
    }
}

// ───────────── 錄影 ─────────────

pub async fn record_start(app: &App, config: RecordConfig) -> Result<()> {
    if app.exporter.running() {
        return Err(Error::config("正在製作加速版 / GIF，請等完成再開始錄影"));
    }
    app.recorder.start(config).await
}

/// 儲存設定：ui = 操作視窗的設定（JSON），config = 系統匣「開始錄影」用的錄影設定。回傳新的版本號
pub fn save_settings(app: &App, ui: Option<Value>, config: Option<&RecordConfig>) -> u64 {
    let config = config.and_then(|c| serde_json::to_value(c).ok());
    app.settings.save(crate::settings::SettingsPatch { ui, config, ..Default::default() }).rev
}

pub fn ffmpeg_download(app: &Arc<App>) -> Result<()> {
    if app.ffmpeg_path().is_some() {
        return Err(Error::config("已經有 FFmpeg 了"));
    }
    app.start_download();
    Ok(())
}

// ───────────── 新版本 ─────────────

/// 檢查新版本的設定與結果
#[derive(Debug, Clone, Default)]
pub struct UpdateState {
    pub enabled: bool,
    pub update: Option<UpdateInfo>,
    pub error: Option<String>,
}

pub fn update_state(app: &App) -> UpdateState {
    UpdateState { enabled: app.check_updates_enabled(), update: app.update(), error: app.update_error() }
}

/// 立即檢查新版本
pub async fn check_update(app: &App) -> Result<Option<UpdateInfo>> {
    app.check_update().await.map_err(|e| Error::config(format!("無法檢查新版本：{e}")))
}

// ───────────── 錄影清單 ─────────────

pub async fn library(app: &App, dir: &str, query: &LibraryQuery) -> LibraryPage {
    let dir = Some(dir.trim()).filter(|d| !d.is_empty()).map(str::to_string).unwrap_or_else(|| app.default_output_dir.clone());
    let ffmpeg = app.ffmpeg_path();
    crate::library::list_library(&app.cache, ffmpeg.as_deref(), Path::new(&dir), query).await
}

/// 縮圖檔（PNG）的路徑；產生失敗回傳 None
pub async fn thumb(app: &App, path: &str) -> Option<PathBuf> {
    let ffmpeg = app.ffmpeg_path().filter(|_| is_abs(path) && ends_with_ci(path, &[".mp4", ".gif", ".png"]) && Path::new(path).exists())?;
    app.thumbs.get(&ffmpeg, path).await
}

/// 錄影或截圖改名（錄影連同加速版 / GIF）；不能改正在錄影或轉檔的檔案。回傳新的完整路徑
pub async fn rename(app: &App, path: &str, name: &str) -> Result<String> {
    if !is_abs(path) || !ends_with_ci(path, &[".mp4", ".png"]) || !Path::new(path).exists() {
        return Err(Error::config(format!("找不到檔案：{path}")));
    }
    let mut busy = exporting(app);
    if let Some(p) = app.recorder.output_path() {
        busy.push(p.display().to_string().to_lowercase());
    }
    let new_path = crate::library::rename_recording(&app.cache, path, name, &busy).await?;
    app.projects.renamed(path, &new_path);
    Ok(new_path)
}

/// 移到資源回收筒（可還原）；只接受 .mp4 / .gif / .png，且不能刪正在轉檔的檔案
pub fn delete(app: &App, paths: &[String]) -> Result<usize> {
    if paths.is_empty() {
        return Err(Error::config("沒有選取檔案"));
    }
    let busy = exporting(app);
    for f in paths {
        if !is_abs(f) || !ends_with_ci(f, &[".mp4", ".gif", ".png"]) || !is_file(f) {
            return Err(Error::config(format!("找不到檔案：{f}")));
        }
        if busy.contains(&f.to_lowercase()) {
            return Err(Error::config("檔案正在轉檔中，無法刪除"));
        }
    }
    crate::recycle::move_to_recycle_bin(paths)?;
    for f in paths {
        app.projects.removed(f);
    }
    Ok(paths.len())
}

// ───────────── 加速版 / GIF ─────────────

#[derive(Debug, Clone, PartialEq)]
pub struct ExportRequest {
    pub source: String,
    pub speed: f64,
    pub keep_audio: bool,
    pub format: ExportFormat,
    pub gif_width: f64,
    pub gif_fps: f64,
    /// 加速版縮小後的寬度；0 = 原尺寸
    pub mp4_width: f64,
}

pub async fn export_start(app: &App, r: &ExportRequest) -> Result<()> {
    if app.recorder.active() {
        return Err(Error::config("錄影中無法製作加速版 / GIF，請先停止錄影"));
    }
    let ctx = app.export_ctx();
    match r.format {
        ExportFormat::Gif => app.exporter.start_gif(&ctx, &r.source, r.speed, r.gif_width, r.gif_fps).await?,
        ExportFormat::Mp4 => app.exporter.start(&ctx, &r.source, r.speed, r.keep_audio, r.mp4_width).await?,
    };
    Ok(())
}

// ───────────── 剪輯 ─────────────

/// 開始剪輯。replace = 取代這個剪輯版（修改之前的剪輯）；project = 介面的剪輯設定與標註，完成後存起來供之後修改
pub async fn cut_start(app: &App, source: &str, spec: &EditSpec, replace: Option<&str>, project: Option<Value>) -> Result<()> {
    if app.recorder.active() {
        return Err(Error::config("錄影中無法剪輯，請先停止錄影"));
    }
    if ![spec.start, spec.end].iter().all(|v| v.is_finite()) {
        return Err(Error::config("剪輯設定格式錯誤"));
    }
    if replace.is_some_and(|r| !is_abs(r)) {
        return Err(Error::config("找不到要取代的剪輯版"));
    }
    let on_saved: Option<crate::exporter::OnSaved> = match project {
        Some(data @ Value::Object(_)) => {
            if serde_json::to_vec(&data).map(|v| v.len()).unwrap_or(usize::MAX) > crate::projects::MAX_PROJECT_BYTES {
                return Err(Error::config("標註資料太大"));
            }
            let (store, src) = (app.projects.clone(), source.to_string());
            Some(Box::new(move |out: &str| {
                if let Err(e) = store.save(out, &src, data) {
                    crate::info!("[剪輯] 無法儲存剪輯設定：{e}");
                }
            }))
        }
        _ => None,
    };
    app.exporter.start_cut(&app.export_ctx(), source, spec, replace, on_saved).await?;
    Ok(())
}

/// 找到的剪輯專案是哪一種
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectMatch {
    /// 開啟的就是剪輯版
    Output,
    /// 開啟的是原始影片：最近一次用它做的剪輯
    Source,
}

#[derive(Debug, Clone)]
pub struct EditProject {
    pub project: Project,
    pub matched: ProjectMatch,
    /// 原始影片的資訊（找不到原片時為 None）
    pub source: Option<LibraryEntry>,
}

/// 之前的剪輯設定：path 是剪輯版時回傳它自己的；是原始影片時回傳最近一次用它做的剪輯
pub async fn edit_project(app: &App, path: &str) -> Option<EditProject> {
    if !is_abs(path) || !ends_with_ci(path, &[".mp4"]) {
        return None;
    }
    let store = app.projects.clone();
    let p = path.to_string();
    let (project, matched) =
        tokio::task::spawn_blocking(move || store.for_output(&p).map(|x| (x, ProjectMatch::Output)).or_else(|| store.latest_for_source(&p).map(|x| (x, ProjectMatch::Source)))).await.ok().flatten()?;
    let source = match (is_file(&project.source), app.ffmpeg_path()) {
        (true, Some(ff)) => app.cache.probe(&ff, &project.source).await.ok().map(|m| LibraryEntry { media: m, exports: vec![] }),
        _ => None,
    };
    Some(EditProject { project, matched, source })
}

// ───────────── 開啟檔案 / 網址 ─────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAction {
    /// 用預設播放器播放
    Play,
    /// 在檔案總管中顯示
    Reveal,
    /// 開啟資料夾（不存在時建立）
    Folder,
}

pub fn open(action: OpenAction, path: &str) -> Result<()> {
    let p = path.trim();
    if !is_abs(p) || p.contains('"') {
        return Err(Error::config("路徑不正確"));
    }
    match action {
        OpenAction::Folder => {
            std::fs::create_dir_all(p)?;
            crate::desktop::open_with_explorer(p, false);
        }
        _ => {
            // 只允許開啟影片與截圖（.mp4 / .gif / .png），避免執行任意檔案
            if !ends_with_ci(p, &[".mp4", ".gif", ".png"]) || !is_file(p) {
                return Err(Error::config("找不到檔案"));
            }
            crate::desktop::open_with_explorer(p, action == OpenAction::Reveal);
        }
    }
    Ok(())
}

/// 用預設瀏覽器開啟外部網址（更新頁面、FFmpeg 下載頁）
pub fn open_url(url: &str) -> Result<()> {
    let u = url.trim();
    let lower = u.to_lowercase();
    let scheme_ok = lower.starts_with("http://") || lower.starts_with("https://");
    let host_ok = u.split_once("://").map(|(_, rest)| !rest.is_empty() && !rest.starts_with('/')).unwrap_or(false);
    if !scheme_ok || !host_ok {
        return Err(Error::config(if u.contains("://") { "只能開啟 http / https 網址" } else { "網址不正確" }));
    }
    if u.chars().any(|c| c == '"' || c.is_whitespace() || c.is_control()) {
        return Err(Error::config("只能開啟 http / https 網址"));
    }
    crate::desktop::open_with_explorer(u, false);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_and_paths_are_checked() {
        assert!(open_url("ftp://x").unwrap_err().message().contains("http"));
        assert!(open_url("notaurl").unwrap_err().message().contains("網址不正確"));
        assert!(open_url("https:///x").is_err());
        assert!(open_url("https://a b").is_err());
        assert!(open(OpenAction::Play, "relative.mp4").unwrap_err().message().contains("路徑"));
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("x.exe");
        std::fs::write(&exe, "x").unwrap();
        assert!(open(OpenAction::Play, &exe.display().to_string()).unwrap_err().message().contains("找不到檔案"));
    }

    #[tokio::test]
    async fn guards_before_touching_files() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::with_data_dir(dir.path().to_path_buf());
        assert!(delete(&app, &[]).unwrap_err().message().contains("沒有選取"));
        assert!(delete(&app, &["relative.mp4".into()]).unwrap_err().message().contains("找不到"));
        assert!(rename(&app, "relative.mp4", "x").await.unwrap_err().message().contains("找不到"));
        let spec = EditSpec { start: 1.0, end: 2.0, removed: vec![], crop: None, overlays: vec![] };
        let src = dir.path().join("Rec.mp4").display().to_string();
        assert!(cut_start(&app, &src, &spec, Some("Rec_cut.mp4"), None).await.unwrap_err().message().contains("找不到要取代"));
        assert!(edit_project(&app, &src).await.is_none());
        assert!(edit_project(&app, "relative.mp4").await.is_none());
    }
}
