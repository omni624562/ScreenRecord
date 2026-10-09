//! 本機網頁介面與 API（只綁 127.0.0.1；路徑與 JSON 格式即網頁介面使用的格式）。

use crate::app::App;
use crate::edit::{CropInput, EditSpec};
use crate::error::Error;
use crate::types::{DownloadStatus, ExportState, ExportStatus, LibraryFilter, LibraryQuery, LibrarySort, RecordConfig, RecorderStatus};
use crate::version::{ui_file, CHANGELOG};
use axum::body::{Body, Bytes};
use axum::extract::{Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use regex::Regex;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, LazyLock};
use std::task::{Context, Poll};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

pub const APP_ID: &str = "screen-recorder";

type Ctx = Arc<Shared>;
type Q = Query<HashMap<String, String>>;

pub struct Shared {
    pub app: Arc<App>,
    pub port: u16,
}

// ───────────── 回應 ─────────────

fn json<T: Serialize>(data: &T) -> Response {
    json_status(data, StatusCode::OK)
}

fn json_status<T: Serialize>(data: &T, status: StatusCode) -> Response {
    let body = serde_json::to_vec(data).unwrap_or_default();
    (status, [(header::CONTENT_TYPE, "application/json;charset=utf-8"), (header::CACHE_CONTROL, "no-store")], body).into_response()
}

fn text(status: StatusCode, body: &'static str) -> Response {
    (status, body).into_response()
}

/// API 錯誤：設定 / 輸入錯誤回 400，其他回 500（並寫進記錄檔）
struct ApiError(Error);

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        ApiError(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        #[derive(Serialize)]
        struct Fail<'a> {
            ok: bool,
            error: &'a str,
        }
        let status = if self.0.is_config() { StatusCode::BAD_REQUEST } else { StatusCode::INTERNAL_SERVER_ERROR };
        if !self.0.is_config() {
            crate::error!("{}", self.0.message());
        }
        json_status(&Fail { ok: false, error: self.0.message() }, status)
    }
}

type ApiResult = Result<Response, ApiError>;

fn body<T: DeserializeOwned>(b: &Bytes) -> Result<T, Error> {
    serde_json::from_slice(b).map_err(|_| Error::config("請求格式錯誤"))
}

/// 與 JS 的 Number() 相同的寬鬆轉換（缺少 = NaN）
fn js_number(v: &Option<Value>) -> f64 {
    match v {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        Some(Value::Number(n)) => n.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                0.0
            } else {
                t.parse().unwrap_or(f64::NAN)
            }
        }
        Some(_) => f64::NAN,
    }
}

/// 與 JS 的 String() 相近：null / 缺少 = 空字串
fn js_string(v: &Option<Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    recorder: RecorderStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    export: Option<ExportStatus>,
    download: DownloadStatus,
    settings_rev: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    update: Option<String>,
}

#[derive(Serialize)]
struct OkStatus {
    ok: bool,
    #[serde(flatten)]
    status: Status,
}

fn status(app: &App) -> Status {
    Status {
        recorder: app.recorder.status(),
        export: app.exporter.status(),
        download: app.downloader.status(),
        settings_rev: app.settings.load().rev,
        update: app.update().map(|u| u.version),
    }
}

fn ok_status(app: &App) -> Response {
    json(&OkStatus { ok: true, status: status(app) })
}

#[derive(Serialize)]
struct Ok {
    ok: bool,
}

const OK: Ok = Ok { ok: true };

static WIN_ABS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-zA-Z]:\\|^\\\\").unwrap());

fn is_abs(p: &str) -> bool {
    WIN_ABS.is_match(p)
}

fn ends_with_ci(p: &str, exts: &[&str]) -> bool {
    let l = p.to_lowercase();
    exts.iter().any(|e| l.ends_with(e))
}

fn is_file(p: &str) -> bool {
    std::fs::metadata(p).map(|m| m.is_file()).unwrap_or(false)
}

// ───────────── 安全檢查 ─────────────

/// 只接受來自本機介面的請求：
/// - Host 必須是 127.0.0.1 / localhost（擋 DNS rebinding）
/// - 非 GET 必須同源且為 JSON（擋其他網站對本機 API 發出的跨站請求）
fn check(port: u16, method: &Method, headers: &HeaderMap) -> Option<Response> {
    let h = |name: header::HeaderName| headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or("");
    let host = h(header::HOST);
    if host != format!("127.0.0.1:{port}") && host != format!("localhost:{port}") {
        return Some(text(StatusCode::FORBIDDEN, "Forbidden"));
    }
    let site = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok());
    if site.is_some_and(|s| s != "same-origin" && s != "none") {
        return Some(text(StatusCode::FORBIDDEN, "Forbidden"));
    }
    if method != Method::GET {
        let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
        if origin.is_some_and(|o| o != format!("http://{host}")) {
            return Some(text(StatusCode::FORBIDDEN, "Forbidden"));
        }
        if !h(header::CONTENT_TYPE).contains("application/json") {
            return Some(text(StatusCode::UNSUPPORTED_MEDIA_TYPE, "Unsupported Media Type"));
        }
    }
    None
}

async fn guard(State(ctx): State<Ctx>, req: Request, next: Next) -> Response {
    if let Some(blocked) = check(ctx.port, req.method(), req.headers()) {
        return blocked;
    }
    next.run(req).await
}

// ───────────── 路由 ─────────────

pub fn router(app: Arc<App>, port: u16) -> Router {
    let ctx: Ctx = Arc::new(Shared { app, port });
    let api = Router::new()
        .route("/api/ping", get(|| async { json(&serde_json::json!({ "app": APP_ID, "pid": std::process::id() })) }))
        .route(
            "/api/env",
            get(|State(c): State<Ctx>| async move {
                // 啟動時視窗比偵測先開：等第一次偵測完成再回覆，介面才不會先顯示「找不到 FFmpeg」
                c.app.wait_ready().await;
                json(&c.app.env())
            }),
        )
        .route(
            "/api/changelog",
            get(|| async { ([(header::CONTENT_TYPE, "text/markdown; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], CHANGELOG).into_response() }),
        )
        .route("/api/settings", get(|State(c): State<Ctx>| async move { json(&c.app.settings.load()) }).post(save_settings))
        .route("/api/ffmpeg/download", post(ffmpeg_download))
        .route(
            "/api/encoder/reset-learned",
            post(|State(c): State<Ctx>| async move {
                c.app.reset_learned_gpu();
                json(&OK)
            }),
        )
        .route("/api/update", get(|State(c): State<Ctx>| async move { json(&update_info(&c.app)) }).post(update_post))
        .route(
            "/api/ffmpeg/cancel",
            post(|State(c): State<Ctx>| async move {
                c.app.downloader.cancel();
                ok_status(&c.app)
            }),
        )
        .route("/api/env/refresh", post(|State(c): State<Ctx>| async move { json(&c.app.refresh(false).await) }))
        .route(
            "/api/status",
            get(|State(c): State<Ctx>| async move {
                c.app.touch();
                json(&status(&c.app))
            }),
        )
        .route("/api/preview", get(preview))
        .route("/api/preview/live", get(live_preview))
        .route("/api/record/start", post(record_start))
        .route("/api/record/pause", post(|State(c): State<Ctx>| async move { c.app.recorder.pause().await.map(|_| ok_status(&c.app)).map_err(ApiError) }))
        .route("/api/record/resume", post(|State(c): State<Ctx>| async move { c.app.recorder.resume().await.map(|_| ok_status(&c.app)).map_err(ApiError) }))
        .route("/api/record/stop", post(|State(c): State<Ctx>| async move { c.app.recorder.stop(None).await.map(|_| ok_status(&c.app)).map_err(ApiError) }))
        .route("/api/library", get(library))
        .route("/api/rename", post(rename))
        .route("/api/delete", post(delete))
        .route("/api/export/start", post(export_start))
        .route("/api/cut/start", post(cut_start))
        .route("/api/thumb", get(thumb))
        .route("/api/media", get(media))
        .route(
            "/api/export/cancel",
            post(|State(c): State<Ctx>| async move {
                // 沒有進行中的工作時也回 ok
                let _ = c.app.exporter.cancel();
                ok_status(&c.app)
            }),
        )
        .route("/api/open-url", post(open_url))
        .route("/api/open", post(open))
        .route("/api/quit", post(quit))
        .route_layer(middleware::from_fn_with_state(ctx.clone(), guard));
    Router::new().merge(api).fallback(static_file).with_state(ctx)
}

/// 綁定連接埠：preferred 被占用時往後找
pub async fn bind(preferred: u16, range: u16) -> Option<(tokio::net::TcpListener, u16)> {
    for port in preferred..preferred.saturating_add(range) {
        if let Ok(l) = tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            let p = l.local_addr().ok()?.port();
            return Some((l, p));
        }
    }
    None
}

pub async fn serve(listener: tokio::net::TcpListener, app: Arc<App>, port: u16) -> std::io::Result<()> {
    axum::serve(listener, router(app, port)).await
}

/// 介面（Bun 打包好的靜態檔，內嵌在執行檔內）
async fn static_file(method: Method, uri: axum::http::Uri) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return text(StatusCode::NOT_FOUND, "Not Found");
    }
    let path = uri.path().trim_start_matches('/');
    let name = if path.is_empty() { "index.html" } else { path };
    match ui_file(name) {
        Some((mime, bytes)) => {
            // 打包後的 js / css 檔名含雜湊，內容不會變；首頁每次重新讀取
            let cache = if name == "index.html" { "no-cache" } else { "public, max-age=31536000, immutable" };
            ([(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, cache)], bytes).into_response()
        }
        None => text(StatusCode::NOT_FOUND, "Not Found"),
    }
}

#[derive(Deserialize)]
struct SettingsBody {
    ui: Option<Value>,
    config: Option<Value>,
}

async fn save_settings(State(c): State<Ctx>, b: Bytes) -> ApiResult {
    let SettingsBody { ui, config } = body(&b)?;
    let truthy = |v: Option<Value>| v.filter(|v| !matches!(v, Value::Null | Value::Bool(false)));
    let saved = c.app.settings.save(crate::settings::SettingsPatch { ui: truthy(ui), config: truthy(config), ..Default::default() });
    Ok(json(&serde_json::json!({ "ok": true, "rev": saved.rev })))
}

async fn ffmpeg_download(State(c): State<Ctx>) -> ApiResult {
    if c.app.ffmpeg_path().is_some() {
        return Err(Error::config("已經有 FFmpeg 了").into());
    }
    c.app.start_download();
    Ok(ok_status(&c.app))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateResponse {
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    update: Option<crate::types::UpdateInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    checked_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn update_info(app: &App) -> UpdateResponse {
    UpdateResponse { enabled: app.check_updates_enabled(), update: app.update(), checked_at: app.update_checked_at(), error: app.update_error() }
}

#[derive(Deserialize)]
struct UpdateBody {
    enabled: Option<Value>,
    check: Option<Value>,
}

async fn update_post(State(c): State<Ctx>, b: Bytes) -> ApiResult {
    let UpdateBody { enabled, check } = body(&b)?;
    if let Some(Value::Bool(on)) = enabled {
        c.app.set_check_updates(on);
    }
    let check = matches!(check, Some(v) if !matches!(v, Value::Null | Value::Bool(false)) && v != 0 && v != "");
    if check {
        if let Err(e) = c.app.check_update().await {
            return Err(Error::config(format!("無法檢查新版本：{e}")).into());
        }
    }
    Ok(json(&update_info(&c.app)))
}

async fn preview(State(c): State<Ctx>, Query(q): Q) -> Response {
    if c.app.ffmpeg_path().is_none() {
        return text(StatusCode::SERVICE_UNAVAILABLE, "ffmpeg not found");
    }
    match c.app.preview(q.get("monitor").map(String::as_str).filter(|s| !s.is_empty())).await {
        Some(img) => ([(header::CONTENT_TYPE, "image/jpeg"), (header::CACHE_CONTROL, "no-store")], img).into_response(),
        None => text(StatusCode::INTERNAL_SERVER_ERROR, "preview failed"),
    }
}

/// 即時預覽串流：保留 FFmpeg 行程直到瀏覽器斷線（串流被丟棄）
struct LiveStream {
    inner: ReaderStream<tokio::process::ChildStdout>,
    _guard: crate::app::LiveGuard,
}

impl futures_core::Stream for LiveStream {
    type Item = std::io::Result<Bytes>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

async fn live_preview(State(c): State<Ctx>, Query(q): Q) -> Response {
    let fps = js_number(&q.get("fps").map(|s| Value::String(s.clone())));
    let fps = if fps.is_nan() || fps == 0.0 { 5.0 } else { fps }.clamp(1.0, 10.0);
    match c.app.live_preview(fps, q.get("monitor").map(String::as_str).filter(|s| !s.is_empty())) {
        Some((out, guard)) => {
            let stream = LiveStream { inner: ReaderStream::new(out), _guard: guard };
            ([(header::CONTENT_TYPE, "multipart/x-mixed-replace;boundary=ffmpeg"), (header::CACHE_CONTROL, "no-store")], Body::from_stream(stream)).into_response()
        }
        None => text(StatusCode::SERVICE_UNAVAILABLE, "ffmpeg not found"),
    }
}

async fn record_start(State(c): State<Ctx>, b: Bytes) -> ApiResult {
    if c.app.exporter.running() {
        return Err(Error::config("正在製作加速版 / GIF，請等完成再開始錄影").into());
    }
    let config: RecordConfig = body(&b)?;
    c.app.recorder.start(config).await?;
    Ok(ok_status(&c.app))
}

async fn library(State(c): State<Ctx>, Query(q): Q) -> ApiResult {
    let dir = q.get("dir").map(|d| d.trim().to_string()).filter(|d| !d.is_empty()).unwrap_or_else(|| c.app.default_output_dir.clone());
    fn enum_of<T: DeserializeOwned>(q: &HashMap<String, String>, k: &str) -> Option<T> {
        q.get(k).and_then(|v| serde_json::from_value(Value::String(v.clone())).ok())
    }
    let num_of = |k: &str| q.get(k).map(|v| js_number(&Some(Value::String(v.clone()))));
    let query = LibraryQuery {
        q: q.get("q").cloned(),
        filter: enum_of(&q, "filter").unwrap_or(LibraryFilter::All),
        sort: enum_of(&q, "sort").unwrap_or(LibrarySort::New),
        page: Some(num_of("page").unwrap_or(1.0)),
        page_size: Some(num_of("pageSize").unwrap_or(30.0)),
        fit_px: num_of("fitPx"),
    };
    let ffmpeg = c.app.ffmpeg_path();
    Ok(json(&crate::library::list_library(&c.app.cache, ffmpeg.as_deref(), Path::new(&dir), &query).await))
}

/// 正在轉檔的來源與輸出（小寫）
fn exporting(app: &App) -> Vec<String> {
    match app.exporter.status() {
        Some(j) if j.state == ExportState::Running => vec![j.source.to_lowercase(), j.output.to_lowercase()],
        _ => vec![],
    }
}

#[derive(Deserialize)]
struct RenameBody {
    path: Option<Value>,
    name: Option<Value>,
}

/// 錄影改名（連同加速版 / GIF）；不能改正在錄影或轉檔的檔案
async fn rename(State(c): State<Ctx>, b: Bytes) -> ApiResult {
    let RenameBody { path, name } = body(&b)?;
    let f = js_string(&path);
    if !is_abs(&f) || !ends_with_ci(&f, &[".mp4"]) || !Path::new(&f).exists() {
        return Err(Error::config(format!("找不到檔案：{f}")).into());
    }
    let mut busy = exporting(&c.app);
    if let Some(p) = c.app.recorder.output_path() {
        busy.push(p.display().to_string().to_lowercase());
    }
    let new_path = crate::library::rename_recording(&c.app.cache, &f, &js_string(&name), &busy).await?;
    Ok(json(&serde_json::json!({ "ok": true, "path": new_path })))
}

#[derive(Deserialize)]
struct DeleteBody {
    paths: Option<Value>,
}

/// 刪除（移到資源回收筒，可還原）；只接受 .mp4 / .gif，且不能刪正在轉檔的檔案
async fn delete(State(c): State<Ctx>, b: Bytes) -> ApiResult {
    let DeleteBody { paths } = body(&b)?;
    let list: Vec<String> = match paths {
        Some(Value::Array(a)) if !a.is_empty() => a.into_iter().map(|v| js_string(&Some(v))).collect(),
        _ => return Err(Error::config("沒有選取檔案").into()),
    };
    let busy = exporting(&c.app);
    for f in &list {
        if !is_abs(f) || !ends_with_ci(f, &[".mp4", ".gif"]) || !is_file(f) {
            return Err(Error::config(format!("找不到檔案：{f}")).into());
        }
        if busy.contains(&f.to_lowercase()) {
            return Err(Error::config("檔案正在轉檔中，無法刪除").into());
        }
    }
    crate::recycle::move_to_recycle_bin(&list)?;
    Ok(json(&serde_json::json!({ "ok": true, "deleted": list.len() })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExportBody {
    source: Option<Value>,
    speed: Option<Value>,
    keep_audio: Option<Value>,
    format: Option<Value>,
    gif_width: Option<Value>,
    gif_fps: Option<Value>,
    /// 加速版縮小後的寬度；0 / 未指定 = 原尺寸
    mp4_width: Option<Value>,
}

async fn export_start(State(c): State<Ctx>, b: Bytes) -> ApiResult {
    if c.app.recorder.active() {
        return Err(Error::config("錄影中無法製作加速版 / GIF，請先停止錄影").into());
    }
    let e: ExportBody = body(&b)?;
    let source = js_string(&e.source);
    let speed = js_number(&e.speed);
    let ctx = c.app.export_ctx();
    if e.format == Some(Value::String("gif".into())) {
        c.app.exporter.start_gif(&ctx, &source, speed, js_number(&e.gif_width), js_number(&e.gif_fps)).await?;
    } else {
        let keep_audio = e.keep_audio != Some(Value::Bool(false));
        let w = js_number(&e.mp4_width);
        c.app.exporter.start(&ctx, &source, speed, keep_audio, if w.is_nan() { 0.0 } else { w }).await?;
    }
    Ok(ok_status(&c.app))
}

#[derive(Deserialize)]
struct CutBody {
    source: Option<Value>,
    spec: Option<Value>,
}

async fn cut_start(State(c): State<Ctx>, b: Bytes) -> ApiResult {
    if c.app.recorder.active() {
        return Err(Error::config("錄影中無法剪輯，請先停止錄影").into());
    }
    let CutBody { source, spec } = body(&b)?;
    let bad = || Error::config("剪輯設定格式錯誤");
    let Some(Value::Object(spec)) = spec else { return Err(bad().into()) };
    let field = |k: &str| js_number(&spec.get(k).cloned());
    let (start, end) = (field("start"), field("end"));
    let Some(Value::Array(removed)) = spec.get("removed") else { return Err(bad().into()) };
    if !start.is_finite() || !end.is_finite() {
        return Err(bad().into());
    }
    let removed = removed
        .iter()
        .filter_map(|r| {
            let a = r.as_array()?;
            let pair = (js_number(&a.first().cloned()), js_number(&a.get(1).cloned()));
            (pair.0.is_finite() && pair.1.is_finite()).then_some(pair)
        })
        .collect();
    let crop = match spec.get("crop") {
        Some(Value::Object(o)) => {
            let g = |k: &str| js_number(&o.get(k).cloned());
            let c = CropInput { x: g("x"), y: g("y"), width: g("width"), height: g("height") };
            [c.x, c.y, c.width, c.height].iter().all(|v| v.is_finite()).then_some(c)
        }
        _ => None,
    };
    let edit = EditSpec { start, end, removed, crop };
    c.app.exporter.start_cut(&c.app.export_ctx(), &js_string(&source), &edit).await?;
    Ok(ok_status(&c.app))
}

async fn thumb(State(c): State<Ctx>, Query(q): Q) -> Response {
    let p = q.get("path").cloned().unwrap_or_default();
    let ffmpeg = c.app.ffmpeg_path();
    let Some(ffmpeg) = ffmpeg.filter(|_| is_abs(&p) && ends_with_ci(&p, &[".mp4", ".gif"]) && Path::new(&p).exists()) else {
        return text(StatusCode::NOT_FOUND, "Not Found");
    };
    let Some(file) = c.app.thumbs.get(&ffmpeg, &p).await else { return text(StatusCode::NOT_FOUND, "Not Found") };
    match tokio::fs::read(&file).await {
        // 網址帶修改時間（v=），內容不會變，可以讓瀏覽器快取
        Ok(img) => ([(header::CONTENT_TYPE, "image/jpeg"), (header::CACHE_CONTROL, "private, max-age=604800, immutable")], img).into_response(),
        Err(_) => text(StatusCode::NOT_FOUND, "Not Found"),
    }
}

/// 解析 Range: bytes=a-b（只支援單一範圍）
fn parse_range(h: &str, size: u64) -> Option<(u64, u64)> {
    let spec = h.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (a, b) = (a.trim(), b.trim());
    if size == 0 {
        return None;
    }
    let (start, end) = if a.is_empty() {
        let n: u64 = b.parse().ok()?;
        if n == 0 {
            return None;
        }
        (size.saturating_sub(n), size - 1)
    } else {
        let s: u64 = a.parse().ok()?;
        let e = if b.is_empty() { size - 1 } else { b.parse::<u64>().ok()?.min(size - 1) };
        (s, e)
    };
    (start <= end && start < size).then_some((start, end))
}

/// 剪輯預覽用：讓瀏覽器直接播放影片檔（支援 Range，可拖曳進度）
async fn media(Query(q): Q, headers: HeaderMap) -> Response {
    let p = q.get("path").cloned().unwrap_or_default();
    if !is_abs(&p) || !p.to_lowercase().ends_with(".mp4") || !is_file(&p) {
        return text(StatusCode::NOT_FOUND, "Not Found");
    }
    let Ok(mut file) = tokio::fs::File::open(&p).await else { return text(StatusCode::NOT_FOUND, "Not Found") };
    let size = file.metadata().await.map(|m| m.len()).unwrap_or(0);
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    let mut resp = match range {
        Some(r) => match parse_range(r, size) {
            Some((start, end)) => {
                if file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
                    return text(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error");
                }
                let len = end - start + 1;
                let mut r = Body::from_stream(ReaderStream::new(file.take(len))).into_response();
                *r.status_mut() = StatusCode::PARTIAL_CONTENT;
                r.headers_mut().insert(header::CONTENT_RANGE, HeaderValue::from_str(&format!("bytes {start}-{end}/{size}")).unwrap());
                r.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(len));
                r
            }
            None => {
                let mut r = StatusCode::RANGE_NOT_SATISFIABLE.into_response();
                r.headers_mut().insert(header::CONTENT_RANGE, HeaderValue::from_str(&format!("bytes */{size}")).unwrap());
                return r;
            }
        },
        None => {
            let mut r = Body::from_stream(ReaderStream::new(file)).into_response();
            r.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(size));
            r
        }
    };
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("video/mp4"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    resp
}

#[derive(Deserialize)]
struct UrlBody {
    url: Option<Value>,
}

/// 用預設瀏覽器開啟外部網址（更新頁面、FFmpeg 下載頁）：操作視窗本身只顯示本機介面
async fn open_url(b: Bytes) -> ApiResult {
    let UrlBody { url } = body(&b)?;
    let u = js_string(&url).trim().to_string();
    let lower = u.to_lowercase();
    let scheme_ok = lower.starts_with("http://") || lower.starts_with("https://");
    let host_ok = u.split_once("://").map(|(_, rest)| !rest.is_empty() && !rest.starts_with('/')).unwrap_or(false);
    if !scheme_ok || !host_ok {
        return Err(Error::config(if u.contains("://") { "只能開啟 http / https 網址" } else { "網址不正確" }).into());
    }
    if u.chars().any(|c| c == '"' || c.is_whitespace() || c.is_control()) {
        return Err(Error::config("只能開啟 http / https 網址").into());
    }
    crate::desktop::open_with_explorer(&u, false);
    Ok(json(&OK))
}

#[derive(Deserialize)]
struct OpenBody {
    action: Option<Value>,
    path: Option<Value>,
}

async fn open(b: Bytes) -> ApiResult {
    let OpenBody { action, path } = body(&b)?;
    let p = js_string(&path).trim().to_string();
    if !is_abs(&p) || p.contains('"') {
        return Err(Error::config("路徑不正確").into());
    }
    let action = js_string(&action);
    if action == "folder" {
        std::fs::create_dir_all(&p).map_err(Error::from)?;
        crate::desktop::open_with_explorer(&p, false);
    } else {
        // 只允許開啟影片（.mp4 / .gif），避免透過此 API 執行任意檔案
        if !ends_with_ci(&p, &[".mp4", ".gif"]) || !is_file(&p) {
            return Err(Error::config("找不到影片檔").into());
        }
        crate::desktop::open_with_explorer(&p, action == "reveal");
    }
    Ok(json(&OK))
}

async fn quit(State(c): State<Ctx>) -> Response {
    let app = c.app.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        app.quit(0).await;
    });
    json(&OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_parsing() {
        assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_range("bytes=900-", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=-100", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=990-2000", 1000), Some((990, 999)));
        assert_eq!(parse_range("bytes=1000-", 1000), None);
        assert_eq!(parse_range("bytes=0-1,5-6", 1000), None);
        assert_eq!(parse_range("items=0-1", 1000), None);
    }

    #[test]
    fn js_number_semantics() {
        assert!(js_number(&None).is_nan());
        assert_eq!(js_number(&Some(Value::Null)), 0.0);
        assert_eq!(js_number(&Some(Value::from(" 12.5 "))), 12.5);
        assert_eq!(js_number(&Some(Value::from(""))), 0.0);
        assert!(js_number(&Some(Value::from("abc"))).is_nan());
        assert_eq!(js_number(&Some(Value::from(true))), 1.0);
    }

    /// 在背景啟動伺服器（資料夾指到暫存目錄），回傳連接埠
    async fn start() -> (u16, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let app = App::with_data_dir(dir.path().to_path_buf());
        let (listener, port) = bind(0, 1).await.unwrap();
        tokio::spawn(serve(listener, app, port));
        (port, dir)
    }

    fn agent() -> ureq::Agent {
        ureq::AgentBuilder::new().build()
    }

    async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        tokio::task::spawn_blocking(f).await.unwrap()
    }

    fn status_of(r: Result<ureq::Response, ureq::Error>) -> (u16, String) {
        match r {
            Ok(r) => (r.status(), r.into_string().unwrap_or_default()),
            Err(ureq::Error::Status(code, r)) => (code, r.into_string().unwrap_or_default()),
            Err(e) => panic!("{e}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn api_contract_and_guards() {
        let (port, dir) = start().await;
        let base = format!("http://127.0.0.1:{port}");
        let b = base.clone();
        let (code, body) = blocking(move || status_of(agent().get(&format!("{b}/api/ping")).call())).await;
        assert_eq!(code, 200);
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["app"], APP_ID);

        // 首頁（內嵌的介面）
        let b = base.clone();
        let (code, body) = blocking(move || status_of(agent().get(&format!("{b}/")).call())).await;
        assert_eq!(code, 200);
        assert!(body.contains("<html"), "{body}");

        // 其他網站來的請求、錯誤的 Host、不是 JSON 的 POST
        let b = base.clone();
        let (code, _) = blocking(move || status_of(agent().get(&format!("{b}/api/status")).set("Sec-Fetch-Site", "cross-site").call())).await;
        assert_eq!(code, 403);
        let b = base.clone();
        let (code, _) = blocking(move || status_of(agent().get(&format!("{b}/api/status")).set("Host", "evil.example").call())).await;
        assert_eq!(code, 403);
        let b = base.clone();
        let (code, _) = blocking(move || status_of(agent().post(&format!("{b}/api/record/stop")).set("Content-Type", "text/plain").send_string("{}"))).await;
        assert_eq!(code, 415);
        let b = base.clone();
        let (code, _) = blocking(move || status_of(agent().post(&format!("{b}/api/record/stop")).set("Origin", "http://evil.example").set("Content-Type", "application/json").send_string(&serde_json::json!({}).to_string()))).await;
        assert_eq!(code, 403);

        // 狀態 JSON 的欄位
        let b = base.clone();
        let (code, body) = blocking(move || status_of(agent().get(&format!("{b}/api/status")).call())).await;
        assert_eq!(code, 200);
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["recorder"]["state"], "idle");
        assert_eq!(v["download"]["phase"], "idle");
        assert!(v.get("export").is_none());
        assert_eq!(v["settingsRev"], 1);

        // 沒有在錄影時停止：400 + 錯誤訊息
        let b = base.clone();
        let (code, body) = blocking(move || status_of(agent().post(&format!("{b}/api/record/stop")).set("Content-Type", "application/json").send_string(&serde_json::json!({}).to_string()))).await;
        assert_eq!(code, 400);
        assert!(body.contains("目前沒有在錄影"), "{body}");

        // 請求格式錯誤
        let b = base.clone();
        let (code, body) = blocking(move || status_of(agent().post(&format!("{b}/api/settings")).set("Content-Type", "application/json").send_string("{bad"))).await;
        assert_eq!(code, 400);
        assert!(body.contains("請求格式錯誤"));

        // 設定存檔與 rev
        let b = base.clone();
        let (code, body) = blocking(move || status_of(agent().post(&format!("{b}/api/settings")).set("Content-Type", "application/json").send_string(&serde_json::json!({ "ui": { "fps": 30 } }).to_string()))).await;
        assert_eq!(code, 200);
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["rev"], 2);
        let b = base.clone();
        let (_, body) = blocking(move || status_of(agent().get(&format!("{b}/api/settings")).call())).await;
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["ui"]["fps"], 30);
        assert!(dir.path().join("settings.json").exists());

        // 只能開 http / https、只能播放影片
        let b = base.clone();
        let (code, _) = blocking(move || status_of(agent().post(&format!("{b}/api/open-url")).set("Content-Type", "application/json").send_string(&serde_json::json!({ "url": "file:///C:/x.exe" }).to_string()))).await;
        assert_eq!(code, 400);
        let b = base.clone();
        let (code, _) = blocking(move || status_of(agent().post(&format!("{b}/api/open")).set("Content-Type", "application/json").send_string(&serde_json::json!({ "action": "play", "path": "C:\\x.exe" }).to_string()))).await;
        assert_eq!(code, 400);

        // 沒有 FFmpeg 時的預覽
        let b = base.clone();
        let (code, _) = blocking(move || status_of(agent().get(&format!("{b}/api/preview")).call())).await;
        assert_eq!(code, 503);

        // 更新說明、不存在的路徑
        let b = base.clone();
        let (code, body) = blocking(move || status_of(agent().get(&format!("{b}/api/changelog")).call())).await;
        assert_eq!(code, 200);
        assert!(!body.is_empty());
        let b = base.clone();
        let (code, _) = blocking(move || status_of(agent().get(&format!("{b}/nope.js")).call())).await;
        assert_eq!(code, 404);
    }
}
