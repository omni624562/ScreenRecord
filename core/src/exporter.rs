//! 轉檔工作：製作加速版、GIF、剪輯。同一時間只跑一個，避免搶 CPU；原檔不變。

use crate::args::{cut_args, export_args, gif_args, EncoderSpec, GifOptions, OverlayInput};
use crate::edit::{cut_file_name, keep_ranges, normalize_crop, total_length, EditSpec, Overlay, OverlayKind};
use crate::error::{Error, Result};
use crate::format::{export_file_name, js_round, num, speed_label, strip_mp4, FPS_MAX, SPEED_MAX, SPEED_MIN};
use crate::library::MediaCache;
use crate::process::{command, last_lines, read_lines};
use crate::types::{ExportFormat, ExportKind, ExportState, ExportStatus, MediaInfo, Rect};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::{oneshot, watch};

fn kind_text(k: ExportKind) -> &'static str {
    match k {
        ExportKind::Speed => "加速版",
        ExportKind::Gif => "GIF",
        ExportKind::Cut => "剪輯",
    }
}

struct Job {
    status: ExportStatus,
    started: Instant,
    ended: Option<Instant>,
    stderr: String,
    canceled: bool,
    cancel: Option<oneshot::Sender<()>>,
}

#[derive(Default)]
struct State {
    job: Option<Job>,
    next_id: u64,
    /// 準備中（讀取影片資訊）也算進行中：避免準備的空檔又開始錄影或第二個轉檔
    starting: bool,
}

/// 轉檔需要的環境：FFmpeg、編碼器、影片資訊快取
#[derive(Clone)]
pub struct ExportCtx {
    pub ffmpeg: Option<PathBuf>,
    pub encoder: Option<EncoderSpec>,
    pub cache: Arc<MediaCache>,
}

#[derive(Clone, Default)]
pub struct Exporter {
    state: Arc<Mutex<State>>,
    /// 工作結束時 +1（測試與程式結束時等待用）
    finished: Arc<Mutex<Option<watch::Sender<u64>>>>,
}

/// 離開時清掉 starting 旗標
struct StartingGuard(Arc<Mutex<State>>);
impl Drop for StartingGuard {
    fn drop(&mut self) {
        self.0.lock().unwrap().starting = false;
    }
}

impl Exporter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn running(&self) -> bool {
        let s = self.state.lock().unwrap();
        s.starting || s.job.as_ref().is_some_and(|j| j.status.state == ExportState::Running)
    }

    pub fn status(&self) -> Option<ExportStatus> {
        let s = self.state.lock().unwrap();
        s.job.as_ref().map(snapshot)
    }

    /// 檢查與開始之間有 await，用旗標讓整段成為一個不可重入的動作
    fn begin(&self) -> Result<StartingGuard> {
        let mut s = self.state.lock().unwrap();
        if s.starting || s.job.as_ref().is_some_and(|j| j.status.state == ExportState::Running) {
            return Err(Error::config("已有轉檔工作進行中，請等它完成"));
        }
        s.starting = true;
        Ok(StartingGuard(self.state.clone()))
    }

    /// 製作加速版；width 為縮小後的寬度（0 = 原尺寸）
    pub async fn start(&self, ctx: &ExportCtx, source: &str, speed: f64, keep_audio: bool, width: f64) -> Result<ExportStatus> {
        let _g = self.begin()?;
        if !speed.is_finite() || !(SPEED_MIN..=SPEED_MAX).contains(&speed) {
            return Err(Error::config(format!("倍率需介於 {}～{}", num(SPEED_MIN), num(SPEED_MAX))));
        }
        let (ffmpeg, enc, info, fps) = prepare(ctx, source).await?;
        let dir = parent(source);
        let output = crate::paths::unique_path(&dir, &strip_mp4(&export_file_name(&file_name(source), speed, ExportFormat::Mp4)), ".mp4");
        let with_audio = keep_audio && info.has_audio == Some(true);
        let w = if width.is_finite() && width >= 160.0 { js_round(width) as i32 } else { 0 };
        let src_w = info.width.map(|x| x as i32);
        let scaled = w > 0 && src_w.is_some_and(|sw| w < sw);
        let args = export_args(source, &output.display().to_string(), speed, fps, &enc, with_audio, Some(w), src_w)?;
        let note = format!("{}×{}{}", speed_label(speed), if with_audio { "，含聲音" } else { "" }, if scaled { format!("，寬 {w}") } else { String::new() });
        self.run(ExportKind::Speed, &ffmpeg, args, source, &output, speed, info.duration_sec.unwrap_or(0.0) / speed, &note)
    }

    /// GIF（可同時加速；無聲音）
    pub async fn start_gif(&self, ctx: &ExportCtx, source: &str, speed: f64, width: f64, fps: f64) -> Result<ExportStatus> {
        let _g = self.begin()?;
        if !speed.is_finite() || !(1.0..=SPEED_MAX).contains(&speed) {
            return Err(Error::config(format!("倍率需介於 1～{}", num(SPEED_MAX))));
        }
        let fps = if fps.is_finite() && js_round(fps) != 0.0 { js_round(fps) } else { 10.0 }.clamp(5.0, 30.0);
        let width = if width.is_finite() && js_round(width) != 0.0 { js_round(width) } else { 640.0 }.clamp(160.0, 1920.0);
        let (ffmpeg, _enc, info, _src_fps) = prepare(ctx, source).await?;
        let output_sec = info.duration_sec.unwrap_or(0.0) / speed;
        let dir = parent(source);
        let name = export_file_name(&file_name(source), speed, ExportFormat::Gif);
        let output = crate::paths::unique_path(&dir, name.trim_end_matches(".gif"), ".gif");
        let opts = GifOptions { fps, width, src_width: info.width.unwrap_or(0) as f64, src_height: info.height.unwrap_or(0) as f64, output_sec };
        let args = gif_args(source, &output.display().to_string(), speed, &opts)?;
        let shown_w = info.width.map(|sw| width.min(sw as f64)).unwrap_or(width);
        let note = format!("{}寬 {}、{} fps", if speed > 1.0 { format!("{}×，", speed_label(speed)) } else { String::new() }, num(shown_w), num(fps));
        self.run(ExportKind::Gif, &ffmpeg, args, source, &output, speed, output_sec, &note)
    }

    /// 剪輯：剪頭尾、刪除中間片段、裁切畫面，另存為 *_cut.mp4
    pub async fn start_cut(&self, ctx: &ExportCtx, source: &str, spec: &EditSpec) -> Result<ExportStatus> {
        let _g = self.begin()?;
        let (ffmpeg, enc, info, fps) = prepare(ctx, source).await?;
        let duration = info.duration_sec.unwrap_or(0.0);
        let keep = keep_ranges(duration, spec);
        let length = total_length(&keep);
        if length < 0.1 {
            return Err(Error::config("剪輯後留下的長度太短"));
        }
        let crop = match spec.crop {
            Some(c) => {
                let (Some(w), Some(h)) = (info.width, info.height) else { return Err(Error::config("無法讀取影片尺寸，不能裁切畫面")) };
                normalize_crop(Some(c), w as i32, h as i32)
            }
            None => None,
        };
        let unchanged = keep.len() == 1 && keep[0].0 == 0.0 && keep[0].1 >= duration - 0.05 && crop.is_none() && spec.overlays.is_empty();
        if unchanged {
            return Err(Error::config("沒有任何剪輯、裁切或標註"));
        }
        let (overlays, temp) = if spec.overlays.is_empty() {
            (Vec::new(), None)
        } else {
            let (Some(w), Some(h)) = (info.width, info.height) else { return Err(Error::config("無法讀取影片尺寸，不能加上標註")) };
            let dir = std::env::temp_dir().join(format!("ScreenRecorder-overlays-{}-{}", std::process::id(), crate::paths::now_ms()));
            match write_overlays(&spec.overlays, w as i32, h as i32, duration, &dir) {
                Ok(list) => (list, Some(dir)),
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&dir);
                    return Err(e);
                }
            }
        };
        let output = crate::paths::unique_path(&parent(source), &strip_mp4(&cut_file_name(&file_name(source))), ".mp4");
        let args = cut_args(source, &output.display().to_string(), &keep, crop, fps, &enc, info.has_audio == Some(true), &overlays)?;
        let note = format!(
            "保留 {} 段{}{}",
            keep.len(),
            crop.map(|c| format!("，裁切 {}×{}", c.width, c.height)).unwrap_or_default(),
            if overlays.is_empty() { String::new() } else { format!("，標註 {} 個", overlays.len()) }
        );
        self.run_with_cleanup(ExportKind::Cut, &ffmpeg, args, source, &output, 1.0, length, &note, temp)
    }

    pub fn cancel(&self) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        match s.job.as_mut() {
            Some(j) if j.status.state == ExportState::Running && j.cancel.is_some() => {
                j.canceled = true;
                let _ = j.cancel.take().unwrap().send(());
                Ok(())
            }
            _ => Err(Error::config("目前沒有進行中的轉檔")),
        }
    }

    /// 程式結束時：中止轉檔並等它刪除未完成的檔案
    pub async fn shutdown(&self) {
        let rx = self.finished.lock().unwrap().as_ref().map(|t| t.subscribe());
        if self.cancel().is_ok() {
            if let Some(mut rx) = rx {
                let _ = rx.changed().await;
            }
        }
    }

    /// 等目前的工作結束（測試用）
    pub async fn wait(&self) {
        let rx = self.finished.lock().unwrap().as_ref().map(|t| t.subscribe());
        if let Some(mut rx) = rx {
            while self.running() {
                if rx.changed().await.is_err() {
                    break;
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn run(&self, kind: ExportKind, ffmpeg: &Path, args: Vec<String>, source: &str, output: &Path, speed: f64, expected_sec: f64, note: &str) -> Result<ExportStatus> {
        self.run_with_cleanup(kind, ffmpeg, args, source, output, speed, expected_sec, note, None)
    }

    /// cleanup：工作結束（完成、失敗、取消）後刪除的暫存資料夾（標註圖檔）
    #[allow(clippy::too_many_arguments)]
    fn run_with_cleanup(&self, kind: ExportKind, ffmpeg: &Path, args: Vec<String>, source: &str, output: &Path, speed: f64, expected_sec: f64, note: &str, cleanup: Option<PathBuf>) -> Result<ExportStatus> {
        let remove_temp = |dir: &Option<PathBuf>| {
            if let Some(d) = dir {
                let _ = std::fs::remove_dir_all(d);
            }
        };
        let mut child = match command(ffmpeg).args(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
            Ok(c) => c,
            Err(e) => {
                remove_temp(&cleanup);
                return Err(Error::config(format!("無法執行 FFmpeg：{e}")));
            }
        };
        let (cancel_tx, mut cancel_rx) = oneshot::channel();
        let output_s = output.display().to_string();
        let id = {
            let mut s = self.state.lock().unwrap();
            s.next_id += 1;
            let id = s.next_id;
            s.job = Some(Job {
                status: ExportStatus {
                    id,
                    kind,
                    state: ExportState::Running,
                    source: source.into(),
                    output: output_s.clone(),
                    speed,
                    progress: 0.0,
                    expected_sec,
                    elapsed_ms: 0,
                    eta_sec: None,
                    bytes: None,
                    message: None,
                },
                started: Instant::now(),
                ended: None,
                stderr: String::new(),
                canceled: false,
                cancel: Some(cancel_tx),
            });
            id
        };
        {
            let mut f = self.finished.lock().unwrap();
            if f.is_none() {
                *f = Some(watch::channel(0).0);
            }
        }
        let label = kind_text(kind);
        crate::info!("[{label}] {} → {}（{note}）", file_name(source), file_name(&output_s));

        let state = self.state.clone();
        let finished = self.finished.clone();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let st_out = state.clone();
        let st_err = state.clone();
        tokio::spawn(async move {
            let progress = async {
                if let Some(out) = stdout {
                    read_lines(out, |line| {
                        let Some((key, value)) = line.split_once('=') else { return };
                        let Ok(v) = value.trim().parse::<f64>() else { return };
                        let mut s = st_out.lock().unwrap();
                        let Some(j) = s.job.as_mut().filter(|j| j.status.id == id) else { return };
                        if key == "out_time_us" && v > 0.0 && expected_sec > 0.0 {
                            j.status.progress = (v / 1e6 / expected_sec).min(0.999);
                        } else if key == "total_size" && v > 0.0 {
                            j.status.bytes = Some(v as u64);
                        }
                    })
                    .await;
                }
            };
            let errors = async {
                if let Some(err) = stderr {
                    read_lines(err, |line| {
                        let mut s = st_err.lock().unwrap();
                        if let Some(j) = s.job.as_mut().filter(|j| j.status.id == id) {
                            j.stderr.push_str(line);
                            j.stderr.push('\n');
                            if j.stderr.len() > 8000 {
                                let cut = j.stderr.len() - 4000;
                                let cut = (cut..j.stderr.len()).find(|&i| j.stderr.is_char_boundary(i)).unwrap_or(0);
                                j.stderr.drain(..cut);
                            }
                        }
                    })
                    .await;
                }
            };
            let wait = async {
                tokio::select! {
                    r = child.wait() => r.ok().and_then(|s| s.code()),
                    _ = &mut cancel_rx => { let _ = child.kill().await; None }
                }
            };
            let (_, _, code) = tokio::join!(progress, errors, wait);
            let message = {
                let mut s = state.lock().unwrap();
                let Some(j) = s.job.as_mut().filter(|j| j.status.id == id) else { return };
                j.ended = Some(Instant::now());
                j.cancel = None;
                let out = Path::new(&j.status.output).to_path_buf();
                if j.canceled {
                    j.status.state = ExportState::Canceled;
                    j.status.message = Some(format!("已取消{label}"));
                    let _ = std::fs::remove_file(&out);
                } else if code == Some(0) && out.exists() {
                    j.status.state = ExportState::Done;
                    j.status.progress = 1.0;
                    if let Ok(m) = std::fs::metadata(&out) {
                        j.status.bytes = Some(m.len());
                    }
                    j.status.message = Some(format!("已儲存 {}", j.status.output));
                } else {
                    j.status.state = ExportState::Error;
                    let last = last_lines(&j.stderr, 3);
                    let why = if last.is_empty() { format!("結束代碼 {}", code.map(|c| c.to_string()).unwrap_or_else(|| "?".into())) } else { last };
                    j.status.message = Some(format!("{label}失敗：{why}"));
                    let _ = std::fs::remove_file(&out);
                }
                j.status.message.clone().unwrap_or_default()
            };
            crate::info!("[{label}] {message}");
            if let Some(d) = &cleanup {
                let _ = std::fs::remove_dir_all(d);
            }
            if let Some(tx) = finished.lock().unwrap().as_ref() {
                tx.send_modify(|n| *n += 1);
            }
        });
        Ok(self.status().expect("剛建立的工作"))
    }
}

/// 標註最多幾個（避免濾鏡圖過大）
const MAX_OVERLAYS: usize = 100;
/// 單一標註圖檔的上限（base64 解碼後）
const MAX_OVERLAY_BYTES: usize = 40 * 1024 * 1024;

/// 把介面送來的標註換算成 FFmpeg 的輸入：PNG 寫進 dir，座標限制在畫面內，時間限制在影片長度內
fn write_overlays(list: &[Overlay], vw: i32, vh: i32, duration: f64, dir: &Path) -> Result<Vec<OverlayInput>> {
    use base64::Engine;
    if list.len() > MAX_OVERLAYS {
        return Err(Error::config(format!("標註最多 {MAX_OVERLAYS} 個")));
    }
    std::fs::create_dir_all(dir)?;
    let mut out = Vec::new();
    for (i, o) in list.iter().enumerate() {
        let nums = [o.x, o.y, o.w, o.h, o.start, o.end];
        if nums.iter().any(|v| !v.is_finite()) {
            return Err(Error::config("標註格式錯誤"));
        }
        let start = o.start.clamp(0.0, duration);
        let end = o.end.clamp(0.0, duration);
        if end - start < 0.01 {
            continue; // 不會出現的標註
        }
        match o.kind {
            OverlayKind::Image => {
                let data = o.png.as_deref().ok_or_else(|| Error::config("標註缺少圖片"))?;
                let data = data.strip_prefix("data:image/png;base64,").unwrap_or(data);
                if data.len() > MAX_OVERLAY_BYTES / 3 * 4 + 4 {
                    return Err(Error::config("標註圖片太大"));
                }
                let bytes = base64::engine::general_purpose::STANDARD.decode(data).map_err(|_| Error::config("標註圖片格式錯誤"))?;
                if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                    return Err(Error::config("標註圖片格式錯誤"));
                }
                let path = dir.join(format!("overlay_{i:03}.png"));
                std::fs::write(&path, bytes)?;
                let x = (o.x.round() as i32).clamp(-vw, vw);
                let y = (o.y.round() as i32).clamp(-vh, vh);
                out.push(OverlayInput::Image { path: path.display().to_string(), x, y, start, end });
            }
            OverlayKind::Blur | OverlayKind::Mosaic => {
                // 限制在畫面內、取偶數（4:2:0 的裁切需要），太小的略過
                let x0 = (o.x.max(0.0) as i32).min(vw) / 2 * 2;
                let y0 = (o.y.max(0.0) as i32).min(vh) / 2 * 2;
                let x1 = ((o.x + o.w).min(vw as f64) as i32).max(x0);
                let y1 = ((o.y + o.h).min(vh as f64) as i32).max(y0);
                let (w, h) = ((x1 - x0) / 2 * 2, (y1 - y0) / 2 * 2);
                if w < 8 || h < 8 {
                    continue;
                }
                out.push(OverlayInput::Blur { rect: Rect { x: x0, y: y0, width: w, height: h }, start, end, mosaic: o.kind == OverlayKind::Mosaic });
            }
        }
    }
    Ok(out)
}

fn snapshot(j: &Job) -> ExportStatus {
    let elapsed = j.ended.unwrap_or_else(Instant::now).duration_since(j.started);
    let mut st = j.status.clone();
    st.elapsed_ms = elapsed.as_millis() as u64;
    st.eta_sec = (st.state == ExportState::Running && st.progress > 0.02).then(|| elapsed.as_secs_f64() * (1.0 - st.progress) / st.progress);
    st
}

fn file_name(p: &str) -> String {
    Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn parent(p: &str) -> PathBuf {
    Path::new(p).parent().map(Path::to_path_buf).unwrap_or_default()
}

async fn prepare(ctx: &ExportCtx, source: &str) -> Result<(PathBuf, EncoderSpec, MediaInfo, f64)> {
    let (Some(ffmpeg), Some(enc)) = (ctx.ffmpeg.clone(), ctx.encoder) else { return Err(Error::config("FFmpeg 無法使用")) };
    if !source.to_lowercase().ends_with(".mp4") || !Path::new(source).is_file() {
        return Err(Error::config("找不到影片檔"));
    }
    let info = ctx.cache.probe(&ffmpeg, source).await?;
    if info.duration_sec.unwrap_or(0.0) <= 0.0 {
        return Err(Error::config("無法讀取影片長度，檔案可能已損壞"));
    }
    let fps = js_round(info.fps.unwrap_or(30.0)).clamp(1.0, FPS_MAX);
    Ok((ffmpeg, enc, info, fps))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::args::ENCODERS;

    /// 假的 ffmpeg：-i 時印出影片資訊；轉檔時寫出輸出檔與進度（最後一個參數是輸出檔）
    fn fake_ffmpeg(dir: &Path, sleep: &str) -> PathBuf {
        let p = dir.join("ffmpeg");
        let script = format!(
            r#"#!/bin/sh
if [ "$2" = "-i" ] && [ $# -eq 3 ]; then
  echo "  Duration: 00:00:10.00, start: 0" >&2
  echo "  Stream #0:0: Video: h264, yuv420p, 1920x1080, 30 fps" >&2
  exit 1
fi
for last; do :; done
echo "out_time_us=2500000"
echo "total_size=1234"
sleep {sleep}
echo data > "$last"
"#
        );
        std::fs::write(&p, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn ctx(ff: PathBuf) -> ExportCtx {
        ExportCtx { ffmpeg: Some(ff), encoder: Some(ENCODERS[0]), cache: Arc::default() }
    }

    #[tokio::test]
    async fn speed_export_runs_to_done() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("Rec_A.mp4");
        std::fs::write(&src, "x").unwrap();
        let ex = Exporter::new();
        let c = ctx(fake_ffmpeg(dir.path(), "0"));
        let st = ex.start(&c, &src.display().to_string(), 4.0, true, 0.0).await.unwrap();
        assert_eq!(st.state, ExportState::Running);
        assert!(st.output.ends_with("Rec_A_4x.mp4"));
        assert!(ex.start(&c, &src.display().to_string(), 4.0, true, 0.0).await.unwrap_err().message().contains("進行中"));
        ex.wait().await;
        let done = ex.status().unwrap();
        assert_eq!(done.state, ExportState::Done, "{:?}", done.message);
        assert_eq!(done.progress, 1.0);
        assert!(dir.path().join("Rec_A_4x.mp4").exists());
        // 第二次：已有同名檔，另存 _2
        let again = ex.start(&c, &src.display().to_string(), 4.0, false, 0.0).await.unwrap();
        assert!(again.output.ends_with("Rec_A_4x_2.mp4"));
        ex.wait().await;
    }

    #[tokio::test]
    async fn cancel_removes_partial_output() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("Rec_B.mp4");
        std::fs::write(&src, "x").unwrap();
        let ex = Exporter::new();
        let st = ex.start_gif(&ctx(fake_ffmpeg(dir.path(), "5")), &src.display().to_string(), 2.0, 640.0, 10.0).await.unwrap();
        assert!(st.output.ends_with("Rec_B_2x.gif"));
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let p = ex.status().unwrap();
        assert_eq!(p.progress, 0.5); // 2.5 秒 / 預期 5 秒
        ex.cancel().unwrap();
        ex.wait().await;
        let s = ex.status().unwrap();
        assert_eq!(s.state, ExportState::Canceled);
        assert_eq!(s.message.as_deref(), Some("已取消GIF"));
        assert!(!dir.path().join("Rec_B_2x.gif").exists());
        assert!(ex.cancel().is_err());
    }

    #[tokio::test]
    async fn cut_validates_spec() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("Rec_C.mp4");
        std::fs::write(&src, "x").unwrap();
        let ex = Exporter::new();
        let c = ctx(fake_ffmpeg(dir.path(), "0"));
        let s = src.display().to_string();
        let unchanged = EditSpec { start: 0.0, end: 10.0, removed: vec![], crop: None, overlays: vec![] };
        assert!(ex.start_cut(&c, &s, &unchanged).await.unwrap_err().message().contains("沒有任何剪輯"));
        let too_short = EditSpec { start: 1.0, end: 1.05, removed: vec![], crop: None, overlays: vec![] };
        assert!(ex.start_cut(&c, &s, &too_short).await.unwrap_err().message().contains("太短"));
        let ok = ex.start_cut(&c, &s, &EditSpec { start: 1.0, end: 9.0, removed: vec![], crop: None, overlays: vec![] }).await.unwrap();
        assert!(ok.output.ends_with("Rec_C_cut.mp4"));
        ex.wait().await;
        assert_eq!(ex.status().unwrap().state, ExportState::Done);
        assert!(!ex.running());
        assert!(ex.start(&ExportCtx { ffmpeg: None, ..c }, &s, 4.0, true, 0.0).await.unwrap_err().message().contains("無法使用"));
    }
}

#[cfg(test)]
mod overlay_tests {
    use super::*;
    use base64::Engine;

    #[allow(clippy::too_many_arguments)]
    fn ov(kind: OverlayKind, x: f64, y: f64, w: f64, h: f64, start: f64, end: f64, png: Option<String>) -> Overlay {
        Overlay { kind, x, y, w, h, start, end, png }
    }

    #[test]
    fn overlays_are_written_and_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let png = b"\x89PNG\r\n\x1a\nrest".to_vec();
        let b64 = format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(&png));
        let list = vec![
            ov(OverlayKind::Image, 10.4, -5.0, 100.0, 40.0, 1.0, 3.0, Some(b64)),
            // 超出畫面：裁到畫面內並取偶數
            ov(OverlayKind::Blur, 1801.0, 1001.0, 500.0, 500.0, -1.0, 99.0, None),
            // 太小：略過
            ov(OverlayKind::Mosaic, 10.0, 10.0, 6.0, 6.0, 0.0, 5.0, None),
            // 時間不在影片內：略過
            ov(OverlayKind::Blur, 0.0, 0.0, 100.0, 100.0, 20.0, 30.0, None),
        ];
        let out = write_overlays(&list, 1920, 1080, 10.0, dir.path()).unwrap();
        assert_eq!(out.len(), 2);
        match &out[0] {
            OverlayInput::Image { path, x, y, start, end } => {
                assert_eq!((*x, *y, *start, *end), (10, -5, 1.0, 3.0));
                assert_eq!(std::fs::read(path).unwrap(), png);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(out[1], OverlayInput::Blur { rect: Rect { x: 1800, y: 1000, width: 120, height: 80 }, start: 0.0, end: 10.0, mosaic: false });
    }

    #[test]
    fn rejects_bad_overlays() {
        let dir = tempfile::tempdir().unwrap();
        let not_png = base64::engine::general_purpose::STANDARD.encode(b"GIF89a");
        let bad = |o: Overlay| write_overlays(&[o], 1920, 1080, 10.0, dir.path()).unwrap_err().message().to_string();
        assert!(bad(ov(OverlayKind::Image, 0.0, 0.0, 10.0, 10.0, 0.0, 1.0, Some(not_png))).contains("格式錯誤"));
        assert!(bad(ov(OverlayKind::Image, 0.0, 0.0, 10.0, 10.0, 0.0, 1.0, None)).contains("缺少圖片"));
        assert!(bad(ov(OverlayKind::Blur, f64::NAN, 0.0, 10.0, 10.0, 0.0, 1.0, None)).contains("格式錯誤"));
        let many = vec![ov(OverlayKind::Blur, 0.0, 0.0, 10.0, 10.0, 0.0, 1.0, None); MAX_OVERLAYS + 1];
        assert!(write_overlays(&many, 1920, 1080, 10.0, dir.path()).unwrap_err().message().contains("最多"));
    }
}
