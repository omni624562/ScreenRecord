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
    app.markers.renamed(path, &new_path);
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
        app.markers.removed(f);
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
    /// 壓縮到這個大小以內（MB）；0 = 不限
    pub mp4_max_mb: f64,
}

pub async fn export_start(app: &App, r: &ExportRequest) -> Result<()> {
    if app.recorder.active() {
        return Err(Error::config("錄影中無法製作加速版 / GIF，請先停止錄影"));
    }
    let ctx = app.export_ctx();
    match r.format {
        ExportFormat::Gif => app.exporter.start_gif(&ctx, &r.source, r.speed, r.gif_width, r.gif_fps).await?,
        ExportFormat::Mp4 => app.exporter.start(&ctx, &r.source, r.speed, r.keep_audio, r.mp4_width, r.mp4_max_mb).await?,
    };
    Ok(())
}

// ───────────── 剪輯 ─────────────

/// 開始剪輯。replace = 取代這個剪輯版（修改之前的剪輯）；project = 介面的剪輯設定與標註，完成後存起來供之後修改
/// 讀取截圖裡的 QR 碼
pub async fn shot_qr(path: &str) -> Result<Vec<String>> {
    let (rgba, w, h) = load_image(path).await?;
    tokio::task::spawn_blocking(move || crate::qr::decode(&rgba, w, h)).await.map_err(|e| Error::other(e.to_string()))
}

/// 合併多支錄影
pub async fn merge_start(app: &App, paths: &[String]) -> Result<()> {
    app.exporter.start_merge(&app.export_ctx(), paths).await?;
    Ok(())
}

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

// ───────────── 錄影中打的點 ─────────────

/// 這支錄影裡打的點（影片的秒數，由小到大）
pub async fn markers(app: &App, path: &str) -> Vec<f64> {
    marks(app, path).await.markers
}

/// 這支錄影裡打的點與滑鼠點擊（依時間排序）
pub async fn marks(app: &App, path: &str) -> crate::recorder::Marks {
    let (store, p) = (app.markers.clone(), path.to_string());
    let mut m: crate::recorder::Marks = tokio::task::spawn_blocking(move || store.for_output(&p)).await.ok().flatten().and_then(|x| serde_json::from_value(x.data).ok()).unwrap_or_default();
    m.markers.retain(|t| t.is_finite() && *t >= 0.0);
    m.markers.sort_by(f64::total_cmp);
    m.clicks.retain(|c| c.iter().all(|v| v.is_finite()) && c[0] >= 0.0);
    m.clicks.sort_by(|a, b| a[0].total_cmp(&b[0]));
    m
}

// ───────────── 截圖編輯 ─────────────

/// 截圖之前的編輯：path 是編輯過的圖時回傳它自己的；是原圖時回傳最近一次用它編輯的
#[derive(Debug, Clone)]
pub struct ShotProjectInfo {
    pub project: Project,
    pub matched: ProjectMatch,
    pub spec: crate::shot_edit::ShotSpec,
    /// 原圖還在
    pub source_ok: bool,
}

pub async fn shot_project(app: &App, path: &str) -> Option<ShotProjectInfo> {
    if !is_abs(path) || !ends_with_ci(path, &[".png"]) {
        return None;
    }
    let store = app.projects.clone();
    let p = path.to_string();
    let (project, matched) =
        tokio::task::spawn_blocking(move || store.for_output(&p).map(|x| (x, ProjectMatch::Output)).or_else(|| store.latest_for_source(&p).map(|x| (x, ProjectMatch::Source)))).await.ok().flatten()?;
    let spec = crate::shot_edit::ShotProject::parse(&project.data)?;
    let source_ok = is_file(&project.source);
    Some(ShotProjectInfo { project, matched, spec, source_ok })
}

/// 讀取截圖（RGBA，不透明）與大小
pub async fn load_image(path: &str) -> Result<(Vec<u8>, u32, u32)> {
    let p = path.to_string();
    tokio::task::spawn_blocking(move || {
        let bytes = std::fs::read(&p).map_err(|e| Error::config(format!("讀不到圖片：{e}")))?;
        let pm = tiny_skia::Pixmap::decode_png(&bytes).map_err(|e| Error::config(format!("讀不到圖片：{e}")))?;
        Ok((crate::shot_edit::straight_rgba(&pm), pm.width(), pm.height()))
    })
    .await
    .map_err(|e| Error::other(e.to_string()))?
}

/// 套用編輯後的圖（原尺寸輸出）
async fn render_shot(source: &str, spec: &crate::shot_edit::ShotSpec) -> Result<tiny_skia::Pixmap> {
    let (rgba, w, h) = load_image(source).await?;
    let spec = spec.clone();
    tokio::task::spawn_blocking(move || crate::shot_edit::render(&rgba, w, h, w, h, &spec, false).ok_or_else(|| Error::config("無法套用編輯"))).await.map_err(|e| Error::other(e.to_string()))?
}

/// 編輯好的截圖複製到剪貼簿（不存檔）
pub async fn shot_copy(source: &str, spec: &crate::shot_edit::ShotSpec) -> Result<(u32, u32)> {
    let pm = render_shot(source, spec).await?;
    let (w, h) = (pm.width(), pm.height());
    let ok = tokio::task::spawn_blocking(move || crate::clipboard::copy_pixmap(&pm)).await.unwrap_or(false);
    if !ok {
        return Err(Error::other("無法複製到剪貼簿"));
    }
    Ok((w, h))
}

/// 文字辨識：spec 有給時辨識套用編輯後的樣子；回傳 (文字, 語言)
pub async fn shot_ocr(path: &str, spec: Option<&crate::shot_edit::ShotSpec>) -> Result<(String, String)> {
    let (rgba, w, h) = match spec {
        Some(s) => {
            let pm = render_shot(path, s).await?;
            (crate::shot_edit::straight_rgba(&pm), pm.width(), pm.height())
        }
        None => load_image(path).await?,
    };
    tokio::task::spawn_blocking(move || crate::ocr::recognize(&rgba, w, h)).await.map_err(|e| Error::other(e.to_string()))?.map_err(Error::config)
}

/// 可以用的攝影機（Windows 的 DirectShow 裝置）；其他平台沒有
pub async fn list_cameras(app: &App) -> Vec<String> {
    if !cfg!(windows) {
        return vec![];
    }
    let Some(ffmpeg) = app.ffmpeg_path() else { return vec![] };
    let r = crate::process::run(&ffmpeg, &crate::args::list_cameras_args(), std::time::Duration::from_secs(10)).await;
    crate::args::parse_cameras(&r.stderr)
}

/// 自動遮個資：在原圖（轉成編輯中的方向）找出 Email、電話、身分證字號、卡號的位置（只有 Windows）
pub async fn shot_find_pii(path: &str, rotate: u32) -> Result<Vec<crate::ocr::Found>> {
    let (rgba, w, h) = load_image(path).await?;
    tokio::task::spawn_blocking(move || {
        let (rgba, w, h) = crate::shot_edit::rotate_rgba(&rgba, w, h, rotate);
        crate::ocr::recognize_words(&rgba, w, h).map(|lines| crate::ocr::find_pii(&lines))
    })
    .await
    .map_err(|e| Error::other(e.to_string()))?
    .map_err(Error::config)
}

/// 釘在桌面（編輯後的樣子；只有 Windows）
pub async fn shot_pin(path: &str, spec: &crate::shot_edit::ShotSpec) -> Result<()> {
    let pm = render_shot(path, spec).await?;
    #[cfg(windows)]
    {
        crate::pin_win::show(crate::shot_edit::straight_rgba(&pm), pm.width(), pm.height());
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = pm;
        Err(Error::config("釘在桌面只支援 Windows"))
    }
}

/// 存成編輯過的圖（原圖保留；replace = 取代之前編輯過的那張），複製到剪貼簿，並記住編輯設定（之後可以再改）
pub async fn shot_save(app: &App, source: &str, spec: &crate::shot_edit::ShotSpec, replace: Option<&str>) -> Result<crate::types::ShotInfo> {
    if !is_abs(source) || !is_file(source) {
        return Err(Error::config("找不到原圖"));
    }
    if replace.is_some_and(|r| !is_abs(r)) {
        return Err(Error::config("找不到要取代的圖"));
    }
    let pm = render_shot(source, spec).await?;
    let out = match replace {
        Some(r) => PathBuf::from(r),
        None => crate::shot_edit::edited_path(Path::new(source)),
    };
    let (o, (w, h)) = (out.clone(), (pm.width(), pm.height()));
    let copied = tokio::task::spawn_blocking(move || -> Result<bool> {
        // 先寫暫存檔再改名：取代時不會留下寫到一半的圖
        let tmp = o.with_extension("png.tmp");
        pm.save_png(&tmp).map_err(|e| Error::other(format!("無法儲存圖片：{e}")))?;
        std::fs::rename(&tmp, &o).map_err(|e| Error::other(format!("無法儲存圖片：{e}")))?;
        Ok(crate::clipboard::copy_pixmap(&pm))
    })
    .await
    .map_err(|e| Error::other(e.to_string()))??;
    let data = serde_json::to_value(crate::shot_edit::ShotProject::new(spec.clone())).unwrap_or(Value::Null);
    if serde_json::to_vec(&data).map(|v| v.len()).unwrap_or(usize::MAX) <= crate::projects::MAX_PROJECT_BYTES {
        if let Err(e) = app.projects.save(&out.display().to_string(), source, data) {
            crate::info!("[截圖] 無法儲存編輯設定：{e}");
        }
    }
    crate::info!("[截圖] 編輯後存成 {}（{w}×{h}{}）", out.display(), if copied { "，已複製到剪貼簿" } else { "" });
    Ok(crate::types::ShotInfo { seq: 0, path: out.display().to_string(), width: w, height: h, copied })
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
        let spec = EditSpec { start: 1.0, end: 2.0, removed: vec![], crop: None, overlays: vec![], ..Default::default() };
        let src = dir.path().join("Rec.mp4").display().to_string();
        assert!(cut_start(&app, &src, &spec, Some("Rec_cut.mp4"), None).await.unwrap_err().message().contains("找不到要取代"));
        assert!(edit_project(&app, &src).await.is_none());
        assert!(edit_project(&app, "relative.mp4").await.is_none());
    }
}
