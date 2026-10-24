//! 錄影清單的縮圖：用 FFmpeg 擷取一張畫面（320px 寬 PNG），存在 %LOCALAPPDATA%\ScreenRecorder\thumbs。
//!
//! 截圖（PNG / JPG）不經 FFmpeg：直接在程式裡解碼、縮小（Windows 上每次啟動 FFmpeg 要幾百毫秒，一頁十幾張會很慢）。
//! 以「路徑 + 大小 + 修改時間」為鍵：檔案被覆寫時自動重做；影片同時最多產生 2 張、截圖 4 張。
//! 要求被取消（關掉清單、換頁）時 future 被丟棄，還在排隊的不會產生；產生失敗的檔案 10 分鐘內不再重試。

use crate::paths::mtime_ms;
use crate::process::run;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{OnceCell, Semaphore};

const MAX_FILES: usize = 2000;
const CONCURRENCY: usize = 2;
const IMAGE_CONCURRENCY: usize = 4;
/// 截圖縮圖的大小上限：寬 320（和影片一樣），長截圖高度最多 960
const IMAGE_MAX: (u32, u32) = (320, 960);
const RETRY_FAILED: Duration = Duration::from_secs(10 * 60);
/// 滑過預覽：幾格畫面、每格多寬
pub const STRIP_FRAMES: usize = 12;
const STRIP_W: u32 = 240;

pub struct Thumbnails {
    dir: PathBuf,
    sem: Arc<Semaphore>,
    image_sem: Arc<Semaphore>,
    inflight: Mutex<HashMap<String, Arc<OnceCell<Option<PathBuf>>>>>,
    /// 產生失敗的鍵 → 失敗時間（損壞的檔案、還在寫入的檔案）
    failed: Mutex<HashMap<String, Instant>>,
}

impl Thumbnails {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, sem: Arc::new(Semaphore::new(CONCURRENCY)), image_sem: Arc::new(Semaphore::new(IMAGE_CONCURRENCY)), inflight: Mutex::default(), failed: Mutex::default() }
    }

    /// 取得影片或截圖縮圖的檔案路徑；產生失敗回傳 None。截圖不需要 FFmpeg
    pub async fn get(&self, ffmpeg: Option<&Path>, video: &str) -> Option<PathBuf> {
        let image = crate::library::is_image_name(video);
        if !image && ffmpeg.is_none() {
            return None;
        }
        let meta = tokio::fs::metadata(video).await.ok()?;
        let key = {
            let mut h = Sha256::new();
            h.update(format!("{}|{}|{}", video.to_lowercase(), meta.len(), mtime_ms(&meta)).as_bytes());
            h.finalize().iter().take(20).map(|b| format!("{b:02x}")).collect::<String>()
        };
        let out = self.dir.join(format!("{key}.png"));
        if out.is_file() {
            return Some(out);
        }
        if self.failed.lock().unwrap_or_else(|e| e.into_inner()).get(&key).is_some_and(|t| t.elapsed() < RETRY_FAILED) {
            return None;
        }
        let cell = self.inflight.lock().unwrap_or_else(|e| e.into_inner()).entry(key.clone()).or_default().clone();
        let result = cell
            .get_or_init(|| async {
                let _permit = if image { self.image_sem.clone() } else { self.sem.clone() }.acquire_owned().await.ok()?;
                let _ = tokio::fs::create_dir_all(&self.dir).await;
                if image {
                    let (src, dst) = (PathBuf::from(video), out.clone());
                    if tokio::task::spawn_blocking(move || image_thumb(&src, &dst)).await.unwrap_or(false) {
                        return Some(out.clone());
                    }
                    let _ = std::fs::remove_file(&out);
                    self.failed.lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone(), Instant::now());
                    return None;
                }
                let ffmpeg = ffmpeg?;
                // FFmpeg 先寫到暫存檔，完整了才改成正式檔名：做到一半當掉不會留下壞掉的縮圖（之後一直被當成做好了）
                let tmp = out.with_extension("tmp.png");
                let done = |tmp: &Path, out: &Path| std::fs::metadata(tmp).map(|m| m.len() > 0).unwrap_or(false) && std::fs::rename(tmp, out).is_ok();
                // 有設定封面（寫在影片裡的 attached_pic）就用封面：0:v 去掉 0:V 剩下的就是封面圖
                if crate::cover::can_embed(video) {
                    let out_s = tmp.display().to_string();
                    let args: Vec<String> =
                        ["-hide_banner", "-loglevel", "error", "-threads", "1", "-i", video, "-map", "0:v", "-map", "-0:V", "-frames:v", "1", "-vf", "scale=320:-2:flags=bilinear", "-y", &out_s]
                            .iter()
                            .map(|s| s.to_string())
                            .collect();
                    let r = run(ffmpeg, &args, Duration::from_secs(20)).await;
                    if r.code == 0 && done(&tmp, &out) {
                        return Some(out.clone());
                    }
                    let _ = std::fs::remove_file(&tmp);
                }
                // 沒有封面：取第 1 秒（避開開頭可能的黑畫面），太短的影片改取第一張；-threads 1 不和錄影搶 CPU
                for at in ["1", "0"] {
                    let out_s = tmp.display().to_string();
                    let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-threads", "1"].iter().map(|s| s.to_string()).collect();
                    args.extend(crate::args::input_at(video, at));
                    args.extend(["-frames:v", "1", "-vf", "scale=320:-2:flags=bilinear", "-y", &out_s].iter().map(|s| s.to_string()));
                    let r = run(ffmpeg, &args, Duration::from_secs(20)).await;
                    if r.code == 0 && done(&tmp, &out) {
                        return Some(out.clone());
                    }
                }
                let _ = std::fs::remove_file(&tmp);
                let mut f = self.failed.lock().unwrap_or_else(|e| e.into_inner());
                if f.len() > 500 {
                    f.clear();
                }
                f.insert(key.clone(), Instant::now());
                None
            })
            .await
            .clone();
        self.inflight.lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
        result
    }

    /// 滑過預覽用的長條圖：平均取 STRIP_FRAMES 格畫面（每格寬 240）左右接起來；一樣以「路徑 + 大小 + 修改時間」快取
    pub async fn strip(&self, ffmpeg: Option<&Path>, video: &str, duration: f64) -> Option<PathBuf> {
        let ffmpeg = ffmpeg?;
        if !(duration.is_finite() && duration > 0.0) {
            return None;
        }
        let meta = tokio::fs::metadata(video).await.ok()?;
        self.make_strip(ffmpeg, video, &format!("strip|{}|{}|{}", video.to_lowercase(), meta.len(), mtime_ms(&meta)), duration).await
    }

    /// 網路上的影片（Jellyfin 的串流網址）的滑過預覽長條圖；id 是快取用的名字（影片編號與大小，網址裡有金鑰不能當名字）
    pub async fn strip_remote(&self, ffmpeg: Option<&Path>, url: &str, id: &str, duration: f64) -> Option<PathBuf> {
        let ffmpeg = ffmpeg?;
        if !(duration.is_finite() && duration > 0.0) {
            return None;
        }
        self.make_strip(ffmpeg, url, &format!("strip|remote|{id}"), duration).await
    }

    async fn make_strip(&self, ffmpeg: &Path, video: &str, name: &str, duration: f64) -> Option<PathBuf> {
        let key = {
            let mut h = Sha256::new();
            h.update(name.as_bytes());
            h.finalize().iter().take(20).map(|b| format!("{b:02x}")).collect::<String>()
        };
        let out = self.dir.join(format!("{key}.png"));
        if out.is_file() {
            return Some(out);
        }
        if self.failed.lock().unwrap_or_else(|e| e.into_inner()).get(&key).is_some_and(|t| t.elapsed() < RETRY_FAILED) {
            return None;
        }
        let cell = self.inflight.lock().unwrap_or_else(|e| e.into_inner()).entry(key.clone()).or_default().clone();
        let result = cell
            .get_or_init(|| async {
                let _permit = self.sem.clone().acquire_owned().await.ok()?;
                let _ = tokio::fs::create_dir_all(&self.dir).await;
                // 先寫暫存檔再重新命名：不會留下寫一半的圖
                let tmp = out.with_extension("tmp.png");
                let args = strip_args(video, duration, &tmp.display().to_string());
                let r = run(ffmpeg, &args, Duration::from_secs(60)).await;
                if r.code == 0 && std::fs::metadata(&tmp).map(|m| m.len() > 0).unwrap_or(false) && std::fs::rename(&tmp, &out).is_ok() {
                    return Some(out.clone());
                }
                crate::info!("[縮圖] 滑過預覽產生失敗：{}", crate::jellyfin::redact(r.stderr.trim()));
                let _ = std::fs::remove_file(&tmp);
                self.failed.lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone(), Instant::now());
                None
            })
            .await
            .clone();
        self.inflight.lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
        result
    }

    /// 啟動時整理：刪掉舊版（2.x）的 JPEG 縮圖；超過上限就刪掉最舊的縮圖
    pub fn prune(&self) {
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return };
        let mut files: Vec<(PathBuf, f64)> = rd
            .flatten()
            .filter(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.ends_with(".jpg") {
                    let _ = std::fs::remove_file(e.path());
                }
                name.ends_with(".png")
            })
            .filter_map(|e| e.metadata().ok().map(|m| (e.path(), mtime_ms(&m))))
            .collect();
        if files.len() <= MAX_FILES {
            return;
        }
        files.sort_by(|a, b| a.1.total_cmp(&b.1));
        let extra = files.len() - MAX_FILES;
        for (p, _) in files.into_iter().take(extra) {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// 滑過預覽的長條圖：每格從影片的 (i + 0.5) / N 處跳過去取一張（快速跳轉，不用解整支影片），縮成寬 240 後左右接起來
fn strip_args(video: &str, duration: f64, out: &str) -> Vec<String> {
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "error"].iter().map(|s| s.to_string()).collect();
    let mut graph = String::new();
    let mut stack = String::new();
    for i in 0..STRIP_FRAMES {
        let t = duration * (i as f64 + 0.5) / STRIP_FRAMES as f64;
        a.extend(["-threads".to_string(), "1".into()]);
        a.extend(crate::args::input_at(video, &format!("{t:.3}")));
        graph.push_str(&format!("[{i}:v]trim=end_frame=1,scale={STRIP_W}:-2:flags=bilinear,setsar=1[v{i}];"));
        stack.push_str(&format!("[v{i}]"));
    }
    graph.push_str(&format!("{stack}hstack=inputs={STRIP_FRAMES}"));
    a.extend(["-filter_complex".to_string(), graph, "-frames:v".into(), "1".into(), "-y".into(), out.into()]);
    a
}

/// 滑過預覽：游標在縮圖上的位置（0～1）→ 第幾格、影片的第幾秒
pub fn strip_pick(frac: f64, duration: f64) -> (usize, f64) {
    let frac = if frac.is_finite() { frac.clamp(0.0, 1.0) } else { 0.0 };
    let i = ((frac * STRIP_FRAMES as f64) as usize).min(STRIP_FRAMES - 1);
    (i, duration.max(0.0) * (i as f64 + 0.5) / STRIP_FRAMES as f64)
}

/// 截圖縮圖：解碼後縮到 IMAGE_MAX 以內（小圖不放大），存成 PNG；先寫暫存檔再重新命名，不會留下寫一半的縮圖
fn image_thumb(src: &Path, dst: &Path) -> bool {
    let Ok(img) = image::open(src) else { return false };
    let (w, h) = (img.width(), img.height());
    let img = if w > IMAGE_MAX.0 || h > IMAGE_MAX.1 { img.thumbnail(IMAGE_MAX.0, IMAGE_MAX.1) } else { img };
    let tmp = dst.with_extension("tmp");
    let ok = img.to_rgba8().save_with_format(&tmp, image::ImageFormat::Png).is_ok() && std::fs::rename(&tmp, dst).is_ok();
    if !ok {
        let _ = std::fs::remove_file(&tmp);
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_picks_frames_and_seeks_each_one() {
        // 12 格：左邊第一格、右邊最後一格，時間是那一格的中間
        assert_eq!(strip_pick(0.0, 120.0), (0, 5.0));
        assert_eq!(strip_pick(1.0, 120.0), (11, 115.0));
        assert_eq!(strip_pick(0.5, 120.0), (6, 65.0));
        assert_eq!(strip_pick(f64::NAN, 120.0).0, 0);
        assert_eq!(strip_pick(-3.0, 120.0).0, 0);
        let a = strip_args("v.mp4", 120.0, "out.png");
        assert_eq!(a.iter().filter(|s| *s == "-i").count(), STRIP_FRAMES);
        assert!(a.windows(2).any(|w| w[0] == "-ss" && w[1] == "5.000"));
        assert!(a.windows(2).any(|w| w[0] == "-ss" && w[1] == "115.000"));
        let g = &a[a.iter().position(|s| s == "-filter_complex").unwrap() + 1];
        assert!(g.ends_with("[v11]hstack=inputs=12"), "{g}");
        assert!(a.ends_with(&["-frames:v".into(), "1".into(), "-y".into(), "out.png".into()]));
    }

    #[test]
    fn prune_keeps_newest() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(MAX_FILES + 3) {
            std::fs::write(dir.path().join(format!("{i:05}.png")), "x").unwrap();
        }
        std::fs::write(dir.path().join("old.jpg"), "x").unwrap();
        Thumbnails::new(dir.path().to_path_buf()).prune();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), MAX_FILES);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failing_ffmpeg_returns_none_and_is_not_retried_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("a.mp4");
        std::fs::write(&video, "not a video").unwrap();
        let t = Thumbnails::new(dir.path().join("thumbs"));
        assert_eq!(t.get(Some(Path::new("false")), &video.display().to_string()).await, None);
        assert_eq!(t.failed.lock().unwrap_or_else(|e| e.into_inner()).len(), 1);
        // 失敗後 10 分鐘內直接回傳 None（不會再執行）
        assert_eq!(t.get(Some(Path::new("false")), &video.display().to_string()).await, None);
    }

    /// 設為封面：有 FFmpeg 時實際跑一次。縮圖改用選的那一格，影片本身（開頭、長度）不變
    #[tokio::test]
    async fn thumbnail_uses_the_cover_set_from_a_frame() {
        let Some(ffmpeg) = std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("ffmpeg")).find(|p| p.is_file())) else { return };
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("a.mp4");
        let v = video.display().to_string();
        // 前 1.5 秒紅色、後 1.5 秒藍色
        let ok = std::process::Command::new(&ffmpeg)
            .args(["-loglevel", "error", "-f", "lavfi", "-i", "color=red:s=320x240:r=30:d=1.5", "-f", "lavfi", "-i", "color=blue:s=320x240:r=30:d=1.5"])
            .args(["-filter_complex", "[0][1]concat=n=2:v=1[v]", "-map", "[v]", "-pix_fmt", "yuv420p", &v])
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let rgb = |p: &Path| image::open(p).unwrap().to_rgb8().get_pixel(40, 40).0;
        let t = Thumbnails::new(dir.path().join("thumbs"));
        let before = t.get(Some(&ffmpeg), &v).await.unwrap();
        assert!(rgb(&before)[0] > 200, "沒有封面：取第 1 秒（紅色）");
        let mtime = std::fs::metadata(&video).unwrap().modified().unwrap();
        crate::cover::set_thumbnail(Some(&ffmpeg), &v, 2.5).await.unwrap();
        assert_eq!(std::fs::metadata(&video).unwrap().modified().unwrap(), mtime, "修改時間保留");
        let after = t.get(Some(&ffmpeg), &v).await.unwrap();
        let c = rgb(&after);
        assert!(c[2] > 200 && c[0] < 60, "縮圖是選的那一格（藍色）：{c:?}");
        // 影片內容不變：開頭還是紅色，再設一次也只有一張封面
        assert!(crate::cover::frame_at(&ffmpeg, &v, 0.2).await.unwrap().pixel(40, 40).unwrap().red() > 200);
        // 播完停在最後（時間 = 影片長度）也設得起來
        crate::cover::set_thumbnail(Some(&ffmpeg), &v, 3.0).await.unwrap();
        let probe = std::process::Command::new(&ffmpeg).args(["-hide_banner", "-i", &v]).output().unwrap();
        let info = String::from_utf8_lossy(&probe.stderr);
        assert_eq!(info.matches("attached pic").count(), 1, "{info}");
        assert_eq!(info.matches("Video:").count(), 2, "{info}");
    }

    #[tokio::test]
    async fn screenshots_need_no_ffmpeg() {
        let dir = tempfile::tempdir().unwrap();
        let t = Thumbnails::new(dir.path().join("thumbs"));
        // 寬圖縮到寬 320；長截圖高度最多 960；小圖不放大
        for (name, w, h, want) in [("wide.png", 1920, 1080, (320, 180)), ("long.png", 800, 6000, (128, 960)), ("small.jpg", 200, 100, (200, 100))] {
            let p = dir.path().join(name);
            image::RgbImage::from_pixel(w, h, image::Rgb([200, 30, 30])).save(&p).unwrap();
            let thumb = t.get(None, &p.display().to_string()).await.expect(name);
            assert_eq!(image::image_dimensions(&thumb).unwrap(), want, "{name}");
        }
        // 壞掉的圖：失敗，不會留下檔案
        let bad = dir.path().join("bad.png");
        std::fs::write(&bad, "not a png").unwrap();
        assert_eq!(t.get(None, &bad.display().to_string()).await, None);
        // 影片沒有 FFmpeg 就不產生
        let video = dir.path().join("a.mp4");
        std::fs::write(&video, "x").unwrap();
        assert_eq!(t.get(None, &video.display().to_string()).await, None);
    }
}
