//! 操作視窗呼叫的動作（原本的本機 HTTP API）：檢查輸入後交給錄影器、轉檔、清單、剪輯專案。
//! 錯誤訊息直接顯示給使用者。

use crate::app::App;
use crate::edit::EditSpec;
use crate::error::{Error, Result};
use crate::projects::Project;
use crate::types::{ExportFormat, ExportState, LibraryEntry, LibraryPage, LibraryQuery, RecordConfig, UpdateInfo};
use crate::{tr, trf};
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

/// 開始錄影：錄影優先，背景的轉檔先讓出來（見 yield_jobs）
pub async fn record_start(app: &App, config: RecordConfig) -> Result<()> {
    yield_jobs(app).await;
    app.recorder.start(config).await
}

/// 要開始錄影了：錄影時轉檔會搶 CPU、讓錄影掉格，所以錄完自動處理先暫停（錄完再接著做），
/// 手動開始的轉檔（製作 GIF、剪輯…）停止。回傳被停止的手動轉檔是什麼（例如「製作 GIF」），讓系統匣通知
pub async fn yield_jobs(app: &App) -> Option<String> {
    let paused = app.after_yield();
    let mut stopped = None;
    if let Some(e) = app.exporter.status().filter(|e| e.state == ExportState::Running && !app.after_owns_export(e.id)) {
        let what = job_label(&e);
        crate::info!("[錄影] 為了開始錄影，停止{what}（{}）", e.output);
        let _ = app.exporter.cancel();
        stopped = Some(what);
    }
    if paused || stopped.is_some() {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), app.exporter.wait()).await;
    }
    stopped
}

/// 轉檔工作的白話名稱：「製作 GIF」「剪輯」「製作 4× 加速版」…
pub fn job_label(e: &crate::types::ExportStatus) -> String {
    use crate::types::ExportKind;
    let speed = if e.speed > 1.0 { format!(" {}×", crate::format::speed_label(e.speed)) } else { String::new() };
    match e.kind {
        ExportKind::Cut => tr!("剪輯", "Editing").to_string(),
        ExportKind::Merge => tr!("合併錄影", "Merging recordings").to_string(),
        ExportKind::Convert => tr!("轉成 MP4", "Converting to MP4").to_string(),
        ExportKind::Upright => tr!("轉好畫面另存", "Saving an upright copy").to_string(),
        ExportKind::Gif => trf!("製作 GIF{speed}", "Making a GIF{speed}"),
        ExportKind::Webp => trf!("製作 WebP 動圖{speed}", "Making a WebP{speed}"),
        // 原速：只壓縮
        ExportKind::Speed if e.speed <= 1.0 => tr!("製作壓縮版", "Making a compressed copy").to_string(),
        ExportKind::Speed => trf!("製作 {}× 加速版", "Making a {}× sped-up video", crate::format::speed_label(e.speed)),
    }
}

/// 儲存設定：ui = 操作視窗的設定（JSON），config = 系統匣「開始錄影」用的錄影設定。回傳新的版本號
pub fn save_settings(app: &App, ui: Option<Value>, config: Option<&RecordConfig>) -> u64 {
    let config = config.and_then(|c| serde_json::to_value(c).ok());
    crate::http::configure(ui.as_ref());
    app.settings.save(crate::settings::SettingsPatch { ui, config, ..Default::default() }).rev
}

pub fn ffmpeg_download(app: &Arc<App>) -> Result<()> {
    if app.ffmpeg_path().is_some() {
        return Err(Error::config(tr!("已經有 FFmpeg 了", "FFmpeg is already installed")));
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
    app.check_update().await.map_err(|e| Error::config(trf!("無法檢查新版本：{e}", "Couldn't check for updates: {e}")))
}

// ───────────── 錄影清單 ─────────────

pub async fn library(app: &App, dir: &str, query: &LibraryQuery) -> LibraryPage {
    let dir = Some(dir.trim()).filter(|d| !d.is_empty()).map(str::to_string).unwrap_or_else(|| app.default_output_dir.clone());
    let ffmpeg = app.ffmpeg_path();
    let src = shot_sources(app);
    let query = LibraryQuery { subdirs: list_subdirs(app), ..query.clone() };
    crate::library::list_library_with(&app.cache, ffmpeg.as_deref(), Path::new(&dir), &query, Some(&src)).await
}

/// 清單也列出子資料夾裡的錄影與截圖（設定的 ui.listSubdirs，預設是）
pub fn list_subdirs(app: &App) -> bool {
    app.settings.load().ui.as_ref().and_then(|u| u.get(LIST_SUBDIRS_KEY)).and_then(serde_json::Value::as_bool).unwrap_or(true)
}

/// 設定裡「也列出子資料夾」的欄位名稱（操作視窗的設定存在 ui 底下）
pub const LIST_SUBDIRS_KEY: &str = "listSubdirs";

/// 在背景建立截圖的文字索引（這些資料夾裡的截圖，新的先做）；已經在做時馬上回傳
pub async fn ocr_index(app: &App, dirs: Vec<String>) {
    let dirs = crate::library::expand_dirs(dirs, list_subdirs(app)).await;
    let mut paths: Vec<(f64, String)> = vec![];
    for d in &dirs {
        for p in crate::library::images_in(Path::new(d)) {
            let t = std::fs::metadata(&p).map(|m| crate::paths::mtime_ms(&m)).unwrap_or(0.0);
            paths.push((t, p.display().to_string()));
        }
    }
    paths.sort_by(|a, b| b.0.total_cmp(&a.0));
    let paths: Vec<String> = paths.into_iter().map(|(_, p)| p).collect();
    // 清理要檢查每張截圖在不在（存在 NAS 上時要等網路）：不要佔住非同步的執行緒
    let ocr = app.ocr.clone();
    let _ = tokio::task::spawn_blocking(move || ocr.prune()).await;
    // 同時在背景算每張的長相指紋與主要顏色（瀏覽欄的「顏色」、相關截圖、找相似的截圖都用得到）
    let hashes = app.hashes.clone();
    let all = paths.clone();
    let colors = tokio::task::spawn_blocking(move || {
        hashes.fingerprints(&all);
    });
    app.ocr
        .build(paths, |p| async move {
            // 開發測試（非 Windows）：SCREENRECORDER_FAKE_OCR=name 時把檔名當成圖裡的字
            // SCREENRECORDER_FAKE_OCR=sidecar 時讀同名的 .txt（沒有時用檔名）
            match std::env::var("SCREENRECORDER_FAKE_OCR").as_deref() {
                Ok("name") if cfg!(debug_assertions) => return Ok(file_stem(&p)),
                Ok("sidecar") if cfg!(debug_assertions) => return Ok(std::fs::read_to_string(format!("{p}.txt")).unwrap_or_else(|_| file_stem(&p))),
                _ => {}
            }
            let (rgba, w, h) = load_image(&p).await.map_err(|e| e.message().to_string())?;
            tokio::task::spawn_blocking(move || crate::ocr::recognize(&rgba, w, h).map(|(t, _)| t)).await.map_err(|e| e.to_string())?
        })
        .await;
    let _ = colors.await;
}

fn file_stem(p: &str) -> String {
    Path::new(p).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

fn shot_sources(app: &App) -> crate::facets::Sources<'_> {
    crate::facets::Sources { ocr: &app.ocr, meta: &app.shot_meta, hashes: &app.hashes, pins: &app.pins, vocab: &app.vocab }
}

/// 搜尋框打字時的建議：圖裡真的有、包含這幾個字的詞與張數（最多 limit 個）
pub fn shot_suggest(app: &App, query: &str, limit: usize) -> Vec<(String, usize)> {
    crate::facets::suggest(&app.vocab.get(&app.ocr), query, limit)
}

/// 和這張相關的截圖（同一個程式、時間接近、有共同的詞、長得像），最相關的在前面，最多 limit 張
pub async fn related_shots(app: &App, path: &str, dirs: Vec<String>, limit: usize) -> Vec<crate::types::MediaInfo> {
    let dirs = crate::library::expand_dirs(dirs, list_subdirs(app)).await;
    let all = crate::library::shots_with_time(&app.cache, &dirs).await;
    let src = shot_sources(app);
    let prep = crate::facets::prepare(&src);
    let me = all.iter().find(|(m, _)| m.path.eq_ignore_ascii_case(path));
    let Some((mm, mt)) = me else { return vec![] };
    let mine = crate::facets::describe(&prep, &mm.path, &mm.name, *mt, mm.width.unwrap_or(0), mm.height.unwrap_or(0));
    let mp = prep.print(&mm.path);
    let mut scored: Vec<(f64, crate::types::MediaInfo)> = all
        .iter()
        .filter(|(m, _)| !m.path.eq_ignore_ascii_case(path))
        .map(|(m, t)| {
            let f = crate::facets::describe(&prep, &m.path, &m.name, *t, m.width.unwrap_or(0), m.height.unwrap_or(0));
            (crate::facets::related_score(&prep, (&mine, *mt, mp), (&f, *t, prep.print(&m.path))), m.clone())
        })
        .filter(|(s, _)| *s >= 3.0)
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored.into_iter().take(limit).map(|(_, m)| m).collect()
}

/// 月曆：某個月每一天的截圖張數與那天的常見詞
pub async fn shot_calendar(app: &App, dirs: Vec<String>, year: i32, month: u32) -> Vec<crate::facets::CalDay> {
    let dirs = crate::library::expand_dirs(dirs, list_subdirs(app)).await;
    let all = crate::library::shots_with_time(&app.cache, &dirs).await;
    let src = shot_sources(app);
    let prep = crate::facets::prepare(&src);
    let items: Vec<crate::facets::Facets> = all.iter().map(|(m, t)| crate::facets::describe(&prep, &m.path, &m.name, *t, m.width.unwrap_or(0), m.height.unwrap_or(0))).collect();
    crate::facets::calendar(&prep, &items, year, month)
}

/// 清理舊截圖：這些資料夾裡超過 days 天的截圖（由舊到新）
pub async fn old_shots(app: &App, dirs: Vec<String>, days: u32) -> Vec<crate::types::MediaInfo> {
    let cutoff = crate::paths::now_ms() as f64 - days as f64 * 86_400_000.0;
    let dirs = crate::library::expand_dirs(dirs, list_subdirs(app)).await;
    crate::library::shots_before(&app.cache, &dirs, cutoff).await
}

/// 找出相似的截圖：level 0 完全一樣、1 幾乎一樣、2 相似。每組由新到舊；組依最新那張由新到舊
pub async fn similar_shots(app: &App, dirs: Vec<String>, level: u8) -> Vec<Vec<crate::types::MediaInfo>> {
    let dirs = crate::library::expand_dirs(dirs, list_subdirs(app)).await;
    let all = crate::library::shots_before(&app.cache, &dirs, f64::INFINITY).await;
    let store = app.hashes.clone();
    tokio::task::spawn_blocking(move || {
        let paths: Vec<String> = all.iter().map(|m| m.path.clone()).collect();
        let prints = store.fingerprints(&paths);
        let (idx, items): (Vec<usize>, Vec<_>) = prints.iter().enumerate().filter_map(|(i, f)| f.map(|f| (i, f))).unzip();
        // all 由舊到新：組內反過來就是由新到舊；組之間依最新那張（在 all 裡的位置）由新到舊排
        let mut groups: Vec<(usize, Vec<crate::types::MediaInfo>)> =
            crate::similar::group(&items, crate::similar::max_diff(level)).into_iter().map(|g| (idx[*g.last().unwrap()], g.into_iter().rev().map(|k| all[idx[k]].clone()).collect())).collect();
        groups.sort_by_key(|g| std::cmp::Reverse(g.0));
        let groups: Vec<Vec<crate::types::MediaInfo>> = groups.into_iter().map(|(_, g)| g).collect();
        crate::info!("[相似截圖] {} 張，{} 組", paths.len(), groups.len());
        groups
    })
    .await
    .unwrap_or_default()
}

/// 資料夾裡有幾張截圖（PNG / JPG，不含子資料夾）
pub async fn count_shots(dir: &str) -> usize {
    let d = PathBuf::from(dir);
    tokio::task::spawn_blocking(move || crate::library::images_in(&d).len()).await.unwrap_or(0)
}

/// 截圖從 from 搬到 to（截圖資料夾）：同名的加 _2；截圖編輯的記錄（原圖在哪裡）跟著更新，之後還能再次編輯
pub async fn move_shots(app: &App, from: &str, to: &str) -> crate::library::MoveReport {
    let (f, t) = (PathBuf::from(from), PathBuf::from(to));
    let r = tokio::task::spawn_blocking(move || crate::library::move_images(&f, &t)).await.unwrap_or_default();
    let (store, pins, meta) = (app.projects.clone(), app.pins.clone(), app.shot_meta.clone());
    let moved = r.moved.clone();
    let _ = tokio::task::spawn_blocking(move || {
        for (a, b) in &moved {
            store.renamed(a, b);
            pins.renamed(a, b);
            meta.renamed(a, b);
        }
    })
    .await;
    crate::info!("[截圖] 從 {from} 搬到 {to}：{} 張，失敗 {} 張", r.moved.len(), r.failed.len());
    for (name, why) in &r.failed {
        crate::info!("[截圖]   沒搬成：{name}（{why}）");
    }
    r
}

/// 縮圖檔（PNG）的路徑；產生失敗回傳 None
pub async fn thumb(app: &App, path: &str) -> Option<PathBuf> {
    if !(is_abs(path) && (ends_with_ci(path, &[".mp4", ".gif", ".webp", ".png", ".jpg", ".jpeg"]) || crate::import::is_other_video(path)) && Path::new(path).exists()) {
        return None;
    }
    // 截圖不需要 FFmpeg
    app.thumbs.get(app.ffmpeg_path().as_deref(), path).await
}

/// 滑過預覽用的長條圖（影片才有）
pub async fn thumb_strip(app: &App, path: &str, duration: f64) -> Option<PathBuf> {
    if !(is_abs(path) && ends_with_ci(path, &[".mp4"]) && Path::new(path).exists()) {
        return None;
    }
    app.thumbs.strip(app.ffmpeg_path().as_deref(), path, duration).await
}

/// 錄影或截圖重新命名（錄影連同加速版 / GIF）；不能改正在錄影或轉檔的檔案。回傳新的完整路徑
pub async fn rename(app: &App, path: &str, name: &str) -> Result<String> {
    if !is_abs(path) || !ends_with_ci(path, &[".mp4", ".png", ".jpg", ".jpeg"]) || !Path::new(path).exists() {
        return Err(Error::config(trf!("找不到檔案：{path}", "File not found: {path}")));
    }
    let mut busy = exporting(app);
    if let Some(p) = app.recorder.output_path() {
        busy.push(p.display().to_string().to_lowercase());
    }
    let new_path = crate::library::rename_recording(&app.cache, path, name, &busy).await?;
    app.projects.renamed(path, &new_path);
    app.markers.renamed(path, &new_path);
    app.pins.renamed(path, &new_path);
    app.shot_meta.renamed(path, &new_path);
    app.recorder.result_renamed(path, &new_path);
    remember_rename(path, &new_path);
    Ok(new_path)
}

/// 改過名的檔案（舊路徑, 新路徑）：清單還沒重讀時按到舊名稱的那一支（例如錄完自動命名之後），照樣找得到
static RENAMES: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());

fn remember_rename(from: &str, to: &str) {
    let mut list = RENAMES.lock().unwrap_or_else(|e| e.into_inner());
    list.retain(|(f, _)| !f.eq_ignore_ascii_case(from));
    list.push((from.to_string(), to.to_string()));
    // 只留最近的
    if list.len() > 200 {
        list.remove(0);
    }
}

/// 這個檔案已經不在、而且是改過名的：現在的路徑（改了好幾次時跟到最後一個）；不是時 None
pub fn renamed_to(path: &str) -> Option<String> {
    if Path::new(path).exists() {
        return None;
    }
    let list = RENAMES.lock().unwrap_or_else(|e| e.into_inner());
    let mut cur = path.to_string();
    for _ in 0..10 {
        match list.iter().rev().find(|(f, _)| f.eq_ignore_ascii_case(&cur)) {
            Some((_, to)) => cur = to.clone(),
            None => break,
        }
    }
    (cur != path && Path::new(&cur).exists()).then_some(cur)
}

/// 移到資源回收筒（可還原）；只接受 .mp4 / .gif / .webp / .webm / .png / .m4a，且不能刪正在轉檔的檔案
pub fn delete(app: &App, paths: &[String]) -> Result<usize> {
    if paths.is_empty() {
        return Err(Error::config(tr!("沒有選取檔案", "No files selected")));
    }
    let busy = exporting(app);
    for f in paths {
        if !is_abs(f) || !ends_with_ci(f, &[".mp4", ".gif", ".webp", ".webm", ".png", ".jpg", ".jpeg", ".m4a"]) || !is_file(f) {
            return Err(Error::config(trf!("找不到檔案：{f}", "File not found: {f}")));
        }
        if busy.contains(&f.to_lowercase()) {
            return Err(Error::config(tr!("檔案正在轉檔中，無法刪除", "The file is being converted and can't be deleted")));
        }
    }
    // 剛關掉的播放器（FFmpeg、mpv）在背景結束，檔案可能還開著一下：失敗時再試，最多約 2 秒
    let mut tries = 0;
    while let Err(e) = crate::recycle::move_to_recycle_bin(paths) {
        tries += 1;
        if tries >= 8 || !paths.iter().all(|f| is_file(f)) {
            return Err(e);
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    for f in paths {
        app.projects.removed(f);
        app.markers.removed(f);
        app.pins.removed(f);
        app.shot_meta.removed(f);
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
    /// 動圖來回播放
    pub bounce: bool,
    /// 加速版縮小後的寬度；0 = 原尺寸
    pub mp4_width: f64,
    /// 壓縮到這個大小以內（MB）；0 = 不限
    pub mp4_max_mb: f64,
    /// 另存新檔選的位置；None = 和原片同一個資料夾、自動取名
    pub output: Option<String>,
    /// 原速、不限大小時依畫質壓縮（存成 *_壓縮.mp4）
    pub quality: Option<crate::args::Quality>,
}

pub async fn export_start(app: &App, r: &ExportRequest) -> Result<()> {
    if app.recorder.active() {
        return Err(Error::config(tr!("錄影中無法製作加速版 / GIF，請先停止錄影", "Can't make a sped-up video / GIF while recording. Stop recording first")));
    }
    let ctx = app.export_ctx();
    let output = r.output.as_deref().map(PathBuf::from);
    match r.format {
        ExportFormat::Gif | ExportFormat::Webp => app.exporter.start_gif_to(&ctx, &r.source, r.speed, r.gif_width, r.gif_fps, r.format, r.bounce, output).await?,
        ExportFormat::Mp4 | ExportFormat::Webm => match r.quality {
            Some(q) if r.mp4_max_mb <= 0.0 => app.exporter.start_quality_to(&ctx, &r.source, r.keep_audio, r.mp4_width, q, r.format, output).await?,
            _ => app.exporter.start_to(&ctx, &r.source, r.speed, r.keep_audio, r.mp4_width, r.mp4_max_mb, r.format, output).await?,
        },
    };
    Ok(())
}

// ───────────── 剪輯 ─────────────

/// 開始剪輯。replace = 取代這個剪輯版（修改之前的剪輯）；project = 介面的剪輯設定與標註，完成後存起來供之後修改
/// 合併多支錄影：依給的順序接成一支；name = 成品的名稱（不含 .mp4）。回傳轉檔工作的編號（完成時用來找到成品）
/// quality：None = 保留原畫質（格式都一樣時直接接起來），Some = 用這個畫質重新壓縮
pub async fn merge_start(app: &App, paths: &[String], name: Option<&str>, quality: Option<crate::args::Quality>) -> Result<u64> {
    if app.recorder.active() {
        return Err(Error::config(tr!("錄影中無法合併，請先停止錄影", "Can't merge while recording. Stop recording first")));
    }
    Ok(app.exporter.start_merge_with(&app.export_ctx(), paths, name, quality).await?.id)
}

/// 旋轉影片（改旋轉標記，不重新壓縮；原檔換掉、修改時間保留）。回傳重新讀的影片資訊（寬高對調了）
pub async fn rotate_video(app: &App, path: &str, turn: crate::rotate::Turn) -> Result<crate::types::MediaInfo> {
    if app.recorder.active() {
        return Err(Error::config(tr!("錄影中無法旋轉影片，請先停止錄影", "Can't rotate a video while recording. Stop recording first")));
    }
    let ff = app.ffmpeg_path();
    let r = crate::rotate::rotate_in_place(ff.as_deref(), path, turn).await;
    app.cache.forget(path);
    r?;
    let ff = ff.ok_or_else(|| Error::config(tr!("找不到 FFmpeg", "FFmpeg not found")))?;
    app.cache.probe(&ff, path).await
}

/// 另存畫面轉好的影片（重新壓縮）：回傳轉檔工作的編號
pub async fn upright_start(app: &App, path: &str) -> Result<u64> {
    if app.recorder.active() {
        return Err(Error::config(tr!("錄影中無法轉檔，請先停止錄影", "Can't convert while recording. Stop recording first")));
    }
    Ok(app.exporter.start_upright(&app.export_ctx(), path).await?.id)
}

/// 匯入影片的結果
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportReport {
    /// 做好的 MP4（轉好的或複製過來的）
    pub done: Vec<String>,
    /// 匯入成功的原檔（與 done 一一對應）
    pub done_src: Vec<String>,
    /// 已經在儲存位置裡的 MP4（不用匯入）
    pub already: usize,
    /// 不是影片、略過的檔案
    pub skipped: usize,
    /// 失敗的檔案與原因
    pub failed: Vec<(String, String)>,
    /// 失敗的原檔路徑（與 failed 一一對應）
    pub failed_src: Vec<String>,
    /// 中途被取消（例如開始錄影）
    pub canceled: bool,
}

impl ImportReport {
    fn ok(&mut self, src: &str, out: String) {
        self.done.push(out);
        self.done_src.push(src.to_string());
    }

    fn fail(&mut self, src: &str, name: String, why: String) {
        self.failed.push((name, why));
        self.failed_src.push(src.to_string());
    }
}

/// 轉成 MP4（清單上的 MOV、MKV…）：存在原檔旁邊，原檔保留。回傳轉檔工作的編號
/// 傳到媒體庫前檢查畫面格式：HEVC、AV1、VP9 這類有些電視、手機不能直接播（Jellyfin 要在 NAS 上即時轉檔，可能卡頓）。
/// 回傳編碼名稱（顯示用，例如「HEVC」）；H.264 或看不出來時 None
pub async fn tv_unfriendly_codec(app: &App, path: &str) -> Option<String> {
    let ffmpeg = app.ffmpeg_path()?;
    let probe = crate::process::run(&ffmpeg, &["-hide_banner".to_string(), "-i".into(), path.to_string()], std::time::Duration::from_secs(30)).await;
    tv_unfriendly(crate::import::video_codec(&probe.stderr).as_deref())
}

/// 不是 H.264 的畫面編碼（顯示用的名稱）；聲音檔、H.264、看不出來時 None
pub fn tv_unfriendly(codec: Option<&str>) -> Option<String> {
    match codec? {
        "h264" | "mjpeg" | "png" => None,
        "hevc" => Some("HEVC".into()),
        c => Some(c.to_ascii_uppercase()),
    }
}

/// 另存一份 H.264 + AAC 的（傳到媒體庫用，任何電視都能直接播）：放在新的暫存資料夾、檔名不變，旁邊的字幕也複製過去。
/// 回傳新檔案；呼叫的人用完把它所在的資料夾刪掉
pub async fn h264_for_library(app: &App, path: &str) -> Result<PathBuf> {
    let (Some(ffmpeg), Some(enc)) = (app.ffmpeg_path(), app.export_ctx().encoder) else {
        return Err(Error::config(tr!("FFmpeg 無法使用", "FFmpeg isn't available")));
    };
    let dir = crate::paths::unique_temp("ScreenRecorder-h264", "");
    std::fs::create_dir_all(&dir).map_err(|e| Error::other(e.to_string()))?;
    let src = Path::new(path);
    let out = dir.join(src.file_name().unwrap_or_default());
    let args = crate::import::convert_args(path, &out.display().to_string(), false, &enc);
    let r = crate::process::run(&ffmpeg, &args, std::time::Duration::from_secs(6 * 3600)).await;
    if r.code != 0 || !out.is_file() {
        let _ = std::fs::remove_dir_all(&dir);
        crate::info!("[傳到資料夾] 轉成 H.264 失敗：{}", r.stderr.trim());
        return Err(Error::other(tr!("轉成 H.264 失敗（詳見記錄檔）", "Couldn't convert to H.264 (see the log file)")));
    }
    // 字幕跟著（傳的時候一起傳）；修改時間和原檔一樣
    for f in crate::after_record::result_files(path, None).into_iter().filter(|f| f.as_path() != src) {
        if let Some(n) = f.file_name() {
            let _ = std::fs::copy(&f, dir.join(n));
        }
    }
    if let Ok(t) = std::fs::metadata(src).and_then(|m| m.modified()) {
        crate::paths::set_mtime(&out, t);
    }
    Ok(out)
}

pub async fn convert_start(app: &App, path: &str) -> Result<u64> {
    if app.recorder.active() {
        return Err(Error::config(tr!("錄影中無法轉檔，請先停止錄影", "Can't convert while recording. Stop recording first")));
    }
    Ok(app.exporter.start_convert(&app.export_ctx(), path, None).await?.id)
}

/// 匯入影片（拖進視窗、「匯入影片…」）：MP4 複製到 dest，其他格式轉成 MP4 存到 dest（原檔都不動）；一支一支依序做。
/// 已經在 dest（或它的子資料夾）裡的 MP4 不重複複製；轉檔進度在錄影面板顯示，錄影時會被停止（canceled）
pub async fn import_videos(app: &App, paths: &[String], dest: &Path) -> ImportReport {
    let mut r = ImportReport::default();
    let _ = tokio::fs::create_dir_all(dest).await;
    let dest_l = dest.display().to_string().to_lowercase();
    for p in paths {
        let name = Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if !crate::import::is_importable(&name) || !Path::new(p).is_file() {
            r.skipped += 1;
            continue;
        }
        if app.recorder.active() {
            r.canceled = true;
            break;
        }
        if !crate::import::is_other_video(&name) {
            if p.to_lowercase().starts_with(&dest_l) {
                r.already += 1;
                continue;
            }
            let stem = Path::new(&name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let to = crate::paths::unique_path(dest, &stem, ".mp4");
            match tokio::fs::copy(p, &to).await {
                Ok(_) => r.ok(p, to.display().to_string()),
                Err(e) => r.fail(p, name, e.to_string()),
            }
            continue;
        }
        let st = match app.exporter.start_convert(&app.export_ctx(), p, Some(dest.to_path_buf())).await {
            Ok(st) => st,
            Err(e) => {
                r.fail(p, name, e.message().to_string());
                continue;
            }
        };
        app.exporter.wait().await;
        match app.exporter.status().filter(|s| s.id == st.id) {
            Some(s) if s.state == ExportState::Done => r.ok(p, s.output),
            Some(s) if s.state == ExportState::Canceled => {
                r.canceled = true;
                break;
            }
            Some(s) => r.fail(p, name, s.message.unwrap_or_default()),
            None => r.fail(p, name, String::new()),
        }
    }
    crate::info!("[匯入] 完成 {} 支、已在儲存位置 {}、略過 {}、失敗 {}{}", r.done.len(), r.already, r.skipped, r.failed.len(), if r.canceled { "（中途停止）" } else { "" });
    r
}

pub async fn cut_start(app: &App, source: &str, spec: &EditSpec, replace: Option<&str>, project: Option<Value>) -> Result<()> {
    cut_start_with(app, source, spec, CutTarget { replace: replace.map(String::from), ..Default::default() }, project).await
}

/// 剪輯存到哪裡、章節
#[derive(Debug, Clone, Default)]
pub struct CutTarget {
    /// 取代這個剪輯版（修改之前的剪輯）
    pub replace: Option<String>,
    /// 存到這裡（另存新檔視窗選的位置）；None = 原影片旁邊的 *_cut.mp4
    pub output: Option<String>,
    /// 章節（剪輯視窗改過標記時）；None = 沿用原影片的章節
    pub chapters: Option<Vec<crate::types::Chapter>>,
}

/// 開始剪輯（見 cut_start）；完成後存剪輯專案並刪掉草稿（失敗、取消時草稿留著，可以回到剪輯）
pub async fn cut_start_with(app: &App, source: &str, spec: &EditSpec, target: CutTarget, project: Option<Value>) -> Result<()> {
    if app.recorder.active() {
        return Err(Error::config(tr!("錄影中無法剪輯，請先停止錄影", "Can't edit while recording. Stop recording first")));
    }
    if ![spec.start, spec.end].iter().all(|v| v.is_finite()) {
        return Err(Error::config(tr!("剪輯設定格式錯誤", "Invalid edit settings")));
    }
    let CutTarget { replace, output, chapters } = target;
    if replace.as_deref().is_some_and(|r| !is_abs(r)) {
        return Err(Error::config(tr!("找不到要取代的剪輯版", "The edited version to replace wasn't found")));
    }
    if output.as_deref().is_some_and(|o| !is_abs(o)) {
        return Err(Error::config(tr!("無法儲存到這個位置，請重新選擇", "Can't save to this location. Choose another one")));
    }
    // 取代的是自己選檔名存的剪輯版（不是 *_cut.mp4）：剪輯視窗存的（有專案）才能取代，當作存到那個位置
    let (replace, output) = match replace {
        Some(r) if !crate::library::is_cut_name(&Path::new(&r).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()) && app.projects.for_output(&r).is_some() => (None, Some(r)),
        r => (r, output),
    };
    let on_saved: Option<crate::exporter::OnSaved> = match project {
        Some(data @ Value::Object(_)) => {
            if serde_json::to_vec(&data).map(|v| v.len()).unwrap_or(usize::MAX) > crate::projects::MAX_PROJECT_BYTES {
                return Err(Error::config(tr!("標註資料太大", "Annotation data is too large")));
            }
            let (store, src) = (app.projects.clone(), source.to_string());
            Some(Box::new(move |out: &str| {
                if let Err(e) = store.save(out, &src, data) {
                    crate::info!("[剪輯] 無法儲存剪輯設定：{e}");
                }
                // 完成了：不用再提示接著剪
                store.drafts().removed(&src);
            }))
        }
        _ => None,
    };
    let opts = crate::exporter::CutOptions { replace, output, chapters, on_saved };
    app.exporter.start_cut_with(&app.export_ctx(), source, spec, opts).await?;
    Ok(())
}

/// 還沒完成的剪輯（草稿）：剪輯中的影片的草稿（影片不在了就沒有）
pub async fn edit_draft(app: &App, video: &str) -> Option<Project> {
    if !is_abs(video) {
        return None;
    }
    let (store, p) = (app.projects.clone(), video.to_string());
    tokio::task::spawn_blocking(move || store.drafts().for_output(&p)).await.ok().flatten()
}

/// 存草稿（剪輯中每隔一段時間、開始輸出時）
pub async fn save_edit_draft(app: &App, video: &str, data: Value) -> Result<()> {
    if !is_abs(video) {
        return Err(Error::config(tr!("影片路徑不正確", "Invalid video path")));
    }
    if serde_json::to_vec(&data).map(|v| v.len()).unwrap_or(usize::MAX) > crate::projects::MAX_PROJECT_BYTES {
        return Err(Error::config(tr!("標註資料太大", "Annotation data is too large")));
    }
    let (store, p) = (app.projects.clone(), video.to_string());
    tokio::task::spawn_blocking(move || store.drafts().save(&p, &p, data)).await.map_err(|e| Error::other(e.to_string()))??;
    Ok(())
}

/// 刪掉草稿（選了「不儲存」）
pub async fn clear_edit_draft(app: &App, video: &str) {
    let (store, p) = (app.projects.clone(), video.to_string());
    let _ = tokio::task::spawn_blocking(move || store.drafts().removed(&p)).await;
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
        (true, Some(ff)) => app.cache.probe(&ff, &project.source).await.ok().map(|m| LibraryEntry { media: m, exports: vec![], hits: vec![] }),
        _ => None,
    };
    Some(EditProject { project, matched, source })
}

// ───────────── 錄影中加的標記 ─────────────

/// 這支錄影裡的標記（影片的秒數，由小到大）
pub async fn markers(app: &App, path: &str) -> Vec<f64> {
    marks(app, path).await.markers
}

/// 這支錄影裡的標記、滑鼠點擊與游標位置（依時間排序）
pub async fn marks(app: &App, path: &str) -> crate::recorder::Marks {
    let (store, p) = (app.markers.clone(), path.to_string());
    let mut m: crate::recorder::Marks = tokio::task::spawn_blocking(move || store.for_output(&p)).await.ok().flatten().and_then(|x| serde_json::from_value(x.data).ok()).unwrap_or_default();
    m.markers.retain(|t| t.is_finite() && *t >= 0.0);
    m.markers.sort_by(f64::total_cmp);
    m.clicks.retain(|c| c.iter().all(|v| v.is_finite()) && c[0] >= 0.0);
    m.clicks.sort_by(|a, b| a[0].total_cmp(&b[0]));
    m.zooms.retain(|c| c.iter().all(|v| v.is_finite()) && c[0] >= 0.0);
    m.zooms.sort_by(|a, b| a[0].total_cmp(&b[0]));
    m.cursor.retain(|c| c.iter().all(|v| v.is_finite()) && c[0] >= 0.0);
    m.cursor.sort_by(|a, b| a[0].total_cmp(&b[0]));
    m.actions.retain(|a| a.t.is_finite() && a.t >= 0.0);
    m.actions.sort_by(|a, b| a.t.total_cmp(&b.t));
    m
}

/// 錄影時記下的操作步驟整理成文字清單；沒有記到時 None
pub async fn steps_text(app: &App, path: &str) -> Option<String> {
    let m = marks(app, path).await;
    if m.actions.is_empty() {
        return None;
    }
    let title = Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    Some(crate::steps_log::to_text(&m.actions, &title))
}

/// 一支錄影最多做成幾步的教學文件
pub const RECORDING_STEPS_MAX: usize = 80;

/// 錄影的操作步驟做成步驟教學（和步驟截圖一樣在操作視窗整理，再做成 HTML / Word / PowerPoint / 影片）：
/// 每一步擷取點下去那一刻的畫面、圈出點的位置，說明用記下的按鈕名稱。回傳幾步；沒有操作步驟時回傳 0
pub async fn steps_from_recording(app: &Arc<App>, path: &str) -> Result<usize> {
    if app.steps_draft().is_some() {
        app.open_ui(crate::app::UiPage::StepsReview);
        return Err(Error::config(tr!(
            "上一次的步驟還沒做成文件：請先在操作視窗做成文件或放棄",
            "The steps from last time haven't been made into a guide yet. Finish or discard them in the main window first"
        )));
    }
    let m = marks(app, path).await;
    let picked: Vec<crate::steps_log::Action> = crate::steps_log::steps(&m.actions).into_iter().take(RECORDING_STEPS_MAX).cloned().collect();
    if picked.is_empty() {
        return Ok(0);
    }
    let ffmpeg = app.ffmpeg_path().ok_or_else(|| Error::config(tr!("找不到 FFmpeg", "FFmpeg not found")))?;
    let video = Path::new(path);
    let dir = video.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = video.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let name = format!("教學_{stem}_{}", crate::paths::timestamp());
    let folder = dir.join(&name);
    tokio::fs::create_dir_all(&folder).await.map_err(|e| Error::config(trf!("無法建立資料夾：{e}", "Couldn't create the folder: {e}")))?;
    let mut steps = Vec::with_capacity(picked.len());
    for (i, a) in picked.iter().enumerate() {
        let file = format!("step_{:02}.png", i + 1);
        let out = folder.join(&file);
        // 點下去之前一點點：看得到要點的東西（點完畫面可能已經換了）
        let t = (a.t - 0.1).max(0.0);
        let args: Vec<String> = ["-hide_banner", "-loglevel", "error"]
            .iter()
            .map(|s| s.to_string())
            .chain(crate::args::input_at(path, &format!("{t:.3}")))
            .chain(["-frames:v", "1", "-update", "1", "-y"].iter().map(|s| s.to_string()))
            .chain(std::iter::once(out.display().to_string()))
            .collect();
        let r = crate::process::run(&ffmpeg, &args, std::time::Duration::from_secs(30)).await;
        if r.code != 0 || !tokio::fs::metadata(&out).await.map(|x| x.len() > 0).unwrap_or(false) {
            crate::info!("[截圖] 操作步驟擷取 {t:.2} 秒失敗：{}", r.stderr.trim());
            continue;
        }
        // 圈出點的位置（同一個時間附近的點擊）
        if a.kind != crate::steps_log::KEY {
            if let Some(c) = m.clicks.iter().filter(|c| (c[0] - a.t).abs() < 0.5).min_by(|x, y| (x[0] - a.t).abs().total_cmp(&(y[0] - a.t).abs())) {
                let (png, x, y) = (out.clone(), c[1], c[2]);
                let _ = tokio::task::spawn_blocking(move || crate::steps::ring_png(&png, x, y)).await;
            }
        }
        steps.push(crate::steps::Step { time: crate::format::short_clock(a.t), window: a.window.clone(), image: format!("{name}/{file}"), text: crate::steps_log::line(a) });
    }
    if steps.is_empty() {
        let _ = tokio::fs::remove_dir_all(&folder).await;
        return Err(Error::other(tr!("無法擷取錄影的畫面", "Couldn't capture frames from the recording")));
    }
    let n = steps.len();
    let date = chrono::Local::now().format("%Y/%m/%d %H:%M").to_string();
    crate::info!("[截圖] 錄影 {path} 的操作步驟做成步驟教學：{n} 步");
    app.steps_review(crate::steps::Draft { dir, name, date, steps });
    Ok(n)
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
    if !is_abs(path) || !ends_with_ci(path, &[".png", ".jpg", ".jpeg"]) {
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
        let bytes = std::fs::read(&p).map_err(|e| Error::config(trf!("讀不到圖片：{e}", "Couldn't read the image: {e}")))?;
        let pm = crate::picture::decode_pixmap(&bytes).ok_or_else(|| Error::config(tr!("讀不到圖片（格式不支援或檔案損壞）", "Couldn't read the image (unsupported format or damaged file)")))?;
        Ok((crate::shot_edit::straight_rgba(&pm), pm.width(), pm.height()))
    })
    .await
    .map_err(|e| Error::other(e.to_string()))?
}

/// 套用編輯後的圖（原尺寸輸出）
async fn render_shot(source: &str, spec: &crate::shot_edit::ShotSpec) -> Result<tiny_skia::Pixmap> {
    let (rgba, w, h) = load_image(source).await?;
    let spec = spec.clone();
    tokio::task::spawn_blocking(move || crate::shot_edit::render(&rgba, w, h, w, h, &spec, false).ok_or_else(|| Error::config(tr!("無法套用編輯", "Couldn't apply the edits"))))
        .await
        .map_err(|e| Error::other(e.to_string()))?
}

/// 編輯好的截圖複製到剪貼簿（不存檔）
pub async fn shot_copy(source: &str, spec: &crate::shot_edit::ShotSpec) -> Result<(u32, u32)> {
    let pm = render_shot(source, spec).await?;
    let (w, h) = (pm.width(), pm.height());
    let ok = tokio::task::spawn_blocking(move || crate::clipboard::copy_pixmap(&pm)).await.unwrap_or(false);
    if !ok {
        return Err(Error::other(tr!("無法複製到剪貼簿", "Couldn't copy to the clipboard")));
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

/// 只留下聲音：在同一個資料夾另存成同名的 .m4a（不重新壓縮，很快）；回傳新檔案的路徑
pub async fn save_audio(app: &App, path: &str) -> Result<String> {
    let ffmpeg = app.ffmpeg_path().ok_or_else(|| Error::config(tr!("找不到 ffmpeg.exe", "ffmpeg.exe not found")))?;
    let src = Path::new(path);
    let (Some(dir), Some(stem)) = (src.parent(), src.file_stem()) else { return Err(Error::config(tr!("檔名不正確", "Invalid file name"))) };
    let out = crate::paths::unique_path(dir, &stem.to_string_lossy(), ".m4a");
    let r = crate::process::run(&ffmpeg, &crate::args::audio_file_args(path, &out.to_string_lossy()), std::time::Duration::from_secs(600)).await;
    if r.code != 0 || !out.is_file() {
        let _ = std::fs::remove_file(&out);
        // FFmpeg 的原始訊息只寫進記錄檔；給使用者看的說明要讓人知道怎麼辦
        crate::info!("[檔案] 無法另存聲音 {path}：{}", r.stderr.trim());
        return Err(Error::config(if r.stderr.contains("matches no streams") {
            tr!("這支影片沒有聲音，無法存成 M4A", "This video has no audio, so it can't be saved as M4A")
        } else {
            tr!("無法存成 M4A（影片可能損壞或正在使用中）", "Couldn't save as M4A (the video may be damaged or in use)")
        }));
    }
    crate::info!("[檔案] 另存聲音 {}", out.display());
    Ok(out.display().to_string())
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
        Err(Error::config(tr!("釘在桌面只支援 Windows", "Pin to desktop is only available on Windows")))
    }
}

/// 存成編輯過的圖（原圖保留；replace = 取代之前編輯過的那張；jpg = 新的一張存成 JPG），複製到剪貼簿，
/// 並記住編輯設定（之後可以再改）
pub async fn shot_save(app: &App, source: &str, spec: &crate::shot_edit::ShotSpec, replace: Option<&str>, jpg: bool) -> Result<crate::types::ShotInfo> {
    if !is_abs(source) || !is_file(source) {
        return Err(Error::config(tr!("找不到原圖", "Original image not found")));
    }
    if replace.is_some_and(|r| !is_abs(r)) {
        return Err(Error::config(tr!("找不到要取代的圖", "The image to replace wasn't found")));
    }
    let out = match replace {
        Some(r) => PathBuf::from(r),
        None => crate::shot_edit::edited_path_as(Path::new(source), if jpg { "jpg" } else { "png" }),
    };
    save_rendered(app, source, spec, out).await
}

/// 另存新檔：存到 out（副檔名是 .jpg / .jpeg 時存成 JPG，品質 90，檔案較小；其他存成 PNG），
/// 複製到剪貼簿，並記住編輯設定
pub async fn shot_save_as(app: &App, source: &str, spec: &crate::shot_edit::ShotSpec, out: &str) -> Result<crate::types::ShotInfo> {
    if !is_abs(source) || !is_file(source) {
        return Err(Error::config(tr!("找不到原圖", "Original image not found")));
    }
    if !is_abs(out) {
        return Err(Error::config(tr!("路徑不正確", "Invalid path")));
    }
    save_rendered(app, source, spec, PathBuf::from(out)).await
}

/// 套用編輯後存到 out（PNG 或 JPG）、複製到剪貼簿、記住編輯設定
async fn save_rendered(app: &App, source: &str, spec: &crate::shot_edit::ShotSpec, out: PathBuf) -> Result<crate::types::ShotInfo> {
    let pm = render_shot(source, spec).await?;
    let jpg = ends_with_ci(&out.display().to_string(), &[".jpg", ".jpeg"]);
    let (o, (w, h)) = (out.clone(), (pm.width(), pm.height()));
    let copied = tokio::task::spawn_blocking(move || -> Result<bool> {
        let fail = |e: String| Error::other(trf!("無法儲存圖片：{e}", "Couldn't save the image: {e}"));
        // 先寫暫存檔再重新命名：取代時不會留下寫到一半的圖
        let tmp = o.with_extension(if jpg { "jpg.tmp" } else { "png.tmp" });
        if jpg {
            let bytes = crate::shot_edit::encode_jpeg(&pm, 90).map_err(fail)?;
            std::fs::write(&tmp, bytes).map_err(|e| fail(e.to_string()))?;
        } else {
            pm.save_png(&tmp).map_err(|e| fail(e.to_string()))?;
        }
        std::fs::rename(&tmp, &o).map_err(|e| fail(e.to_string()))?;
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
    if out.display().to_string() != source {
        app.shot_meta.copy_edited(source, &out.display().to_string());
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
        return Err(Error::config(tr!("路徑不正確", "Invalid path")));
    }
    match action {
        OpenAction::Folder => {
            std::fs::create_dir_all(p)?;
            crate::desktop::open_with_explorer(p, false);
        }
        _ => {
            // 只允許開啟影片、截圖與聲音檔（.mp4 / .gif / .webp / .png / .m4a），避免執行任意檔案
            if !ends_with_ci(p, &[".mp4", ".gif", ".webp", ".webm", ".png", ".jpg", ".jpeg", ".m4a"]) || !is_file(p) {
                return Err(Error::config(tr!("找不到檔案", "File not found")));
            }
            crate::desktop::open_with_explorer(p, action == OpenAction::Reveal);
        }
    }
    Ok(())
}

/// 用 Windows 的選擇資料夾視窗選儲存位置（會等到使用者選好或取消，請在背景執行緒呼叫）；
/// 選好後確認可以在那裡存檔（不存在就建立）。取消時 Ok(None)
pub fn pick_output_dir(current: &str) -> Result<Option<String>> {
    let start = Some(PathBuf::from(current.trim())).filter(|p| p.is_dir());
    let Some(dir) = crate::filepick::pick_folder(tr!("選擇儲存錄影與截圖的資料夾", "Choose a folder for recordings and screenshots"), start).map_err(Error::config)? else {
        return Ok(None);
    };
    check_writable_dir(&dir)?;
    Ok(Some(dir.display().to_string()))
}

/// 確認可以在這個資料夾存檔（不存在就建立）
pub fn check_writable_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| Error::config(trf!("無法建立這個資料夾：{e}", "Couldn't create this folder: {e}")))?;
    let probe = dir.join(format!(".screenrecorder-{}.tmp", crate::paths::now_ms()));
    std::fs::write(&probe, b"").map_err(|e| Error::config(trf!("這個資料夾不能存檔（{e}），請選別的資料夾", "Files can't be saved in this folder ({e}). Choose another folder")))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// 開啟資源回收筒（剛移進去的檔案在那裡按右鍵 →「還原」）
pub fn open_recycle_bin() -> Result<()> {
    if !cfg!(windows) {
        return Err(Error::config(tr!("這個系統沒有資源回收筒", "There's no Recycle Bin on this system")));
    }
    crate::desktop::open_with_explorer("shell:RecycleBinFolder", false);
    Ok(())
}

/// 用預設瀏覽器開啟外部網址（更新頁面、FFmpeg 下載頁）
pub fn open_url(url: &str) -> Result<()> {
    let u = url.trim();
    let lower = u.to_lowercase();
    let scheme_ok = lower.starts_with("http://") || lower.starts_with("https://");
    let host_ok = u.split_once("://").map(|(_, rest)| !rest.is_empty() && !rest.starts_with('/')).unwrap_or(false);
    if !scheme_ok || !host_ok {
        return Err(Error::config(if u.contains("://") { tr!("只能開啟 http / https 網址", "Only http / https links can be opened") } else { tr!("網址不正確", "Invalid URL") }));
    }
    if u.chars().any(|c| c == '"' || c.is_whitespace() || c.is_control()) {
        return Err(Error::config(tr!("只能開啟 http / https 網址", "Only http / https links can be opened")));
    }
    crate::desktop::open_with_explorer(u, false);
    Ok(())
}

/// 想名稱用的線索：字幕（旁邊的 SRT）、畫面上的字（Windows 的文字辨識，錄影取 3 個畫面；截圖用文字索引）、錄影 / 截圖時在用的程式
pub async fn name_hints(app: &App, path: &str) -> crate::naming::Hints {
    let p = Path::new(path);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let date = crate::naming::date_of(&stem)
        .unwrap_or_else(|| std::fs::metadata(p).and_then(|m| m.modified()).map(|t| chrono::DateTime::<chrono::Local>::from(t).format("%m-%d").to_string()).unwrap_or_default());
    let meta = app.shot_meta.get(path).unwrap_or_default();
    let mut h = crate::naming::Hints { app: meta.app, title: meta.title, date, ..Default::default() };
    if crate::library::is_image_name(&p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()) {
        if let Some(t) = app.ocr.text(path).filter(|t| !t.trim().is_empty()) {
            h.screen.push(t);
        }
        return h;
    }
    let srt = p.with_extension("srt");
    if let Ok(cues) = crate::subtitles::read_file(&srt) {
        h.subs = cues.into_iter().map(|c| c.text).collect();
    }
    if cfg!(windows) {
        if let Some(ffmpeg) = app.ffmpeg_path() {
            h.screen = screen_text(app, &ffmpeg, path).await;
        }
    }
    h
}

/// 影片 1/4、1/2、3/4 處畫面上的字
async fn screen_text(app: &App, ffmpeg: &Path, path: &str) -> Vec<String> {
    let Ok(info) = app.cache.probe(ffmpeg, path).await else { return vec![] };
    let (Some(d), Some(w), Some(hh)) = (info.duration_sec, info.width, info.height) else { return vec![] };
    let (ffmpeg, path) = (ffmpeg.to_path_buf(), path.to_string());
    tokio::task::spawn_blocking(move || {
        let (fw, fh) = crate::player::fit_size(w, hh, 1920, 1080);
        [0.25, 0.5, 0.75]
            .iter()
            .filter_map(|f| crate::player::decode_frame(&ffmpeg, &path, d * f, fw, fh, false))
            .filter_map(|rgba| crate::ocr::recognize(&rgba, fw, fh).ok().map(|(t, _)| t))
            .filter(|t| !t.trim().is_empty())
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// 依內容建議一個名稱（不含副檔名）；想不出來時 None
pub async fn suggest_name(app: &App, path: &str) -> Option<String> {
    let h = name_hints(app, path).await;
    crate::naming::suggest(&h)
}

/// 依命名規則（設定裡的「命名規則」）重新命名：只改預設的名稱（Rec_時間、Shot_時間；只錄聲音的錄音不改），回傳新的路徑。
/// 規則裡有 {內容} 時，依內容想名稱（同自動命名）
pub async fn apply_name_rule(app: &App, path: &str, rule: &str) -> Option<String> {
    let p = Path::new(path);
    let stem = p.file_stem()?.to_string_lossy().into_owned();
    if rule.trim().is_empty() || !crate::naming::is_auto_name(&stem) || stem.starts_with("錄音_") {
        return None;
    }
    let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_else(|| "mp4".into());
    let meta = app.shot_meta.get(path).unwrap_or_default();
    let mut c = crate::naming::RuleCtx { app: meta.app, title: meta.title, ..crate::naming::RuleCtx::from_stem(&stem) };
    if crate::naming::rule_uses(rule, 5) {
        let mut h = name_hints(app, path).await;
        h.date.clear();
        c.content = crate::naming::suggest(&h).unwrap_or_default();
    }
    let dir = p.parent()?.to_path_buf();
    let existing: Vec<String> = std::fs::read_dir(&dir).ok()?.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    let name = crate::naming::apply_rule(rule, &c, &ext, &existing)?;
    if name.eq_ignore_ascii_case(&stem) {
        return None;
    }
    for n in 1..=20 {
        let cand = if n == 1 { name.clone() } else { format!("{name}_{n}") };
        match rename(app, path, &cand).await {
            Ok(p) => return Some(p),
            Err(e) if e.message().contains(tr!("已有同名的檔案", "A file with this name already exists")) => continue,
            Err(e) => {
                crate::warn!("[命名規則] 無法重新命名：{}", e.message());
                return None;
            }
        }
    }
    None
}

/// 錄完自動命名：還是預設名稱（Rec_時間）時改成建議的名稱，同名時加 _2、_3；回傳新的路徑
pub async fn auto_rename(app: &App, path: &str) -> Option<String> {
    let stem = Path::new(path).file_stem()?.to_string_lossy().into_owned();
    if !crate::naming::is_auto_name(&stem) {
        return None;
    }
    let name = suggest_name(app, path).await?;
    for n in 1..=9 {
        let cand = if n == 1 { name.clone() } else { format!("{name}_{n}") };
        match rename(app, path, &cand).await {
            Ok(p) => return Some(p),
            Err(e) if e.message().contains(tr!("已有同名的檔案", "A file with this name already exists")) => continue,
            Err(e) => {
                crate::warn!("[自動命名] 無法重新命名：{}", e.message());
                return None;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renamed_files_are_found_by_their_old_path() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (dir.path().join("Rec_1.mp4"), dir.path().join("切換來源.mp4"), dir.path().join("final.mp4"));
        std::fs::write(&c, "x").unwrap();
        let s = |p: &std::path::Path| p.display().to_string();
        remember_rename(&s(&a), &s(&b));
        remember_rename(&s(&b), &s(&c));
        assert_eq!(renamed_to(&s(&a)), Some(s(&c)));
        // 還在的檔案、沒改過名的不動
        assert_eq!(renamed_to(&s(&c)), None);
        assert_eq!(renamed_to(&s(&dir.path().join("other.mp4"))), None);
    }

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
