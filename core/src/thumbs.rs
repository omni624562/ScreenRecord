//! 錄影清單的縮圖：用 FFmpeg 擷取一張畫面（320px 寬 PNG），存在 %LOCALAPPDATA%\ScreenRecorder\thumbs。
//!
//! 以「路徑 + 大小 + 修改時間」為鍵：檔案被覆寫時自動重做；同時最多產生 2 張。
//! 請求被取消（關掉清單、換頁）時 future 被丟棄，還在排隊的不會產生；產生失敗的檔案 10 分鐘內不再重試。

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
const RETRY_FAILED: Duration = Duration::from_secs(10 * 60);

pub struct Thumbnails {
    dir: PathBuf,
    sem: Arc<Semaphore>,
    inflight: Mutex<HashMap<String, Arc<OnceCell<Option<PathBuf>>>>>,
    /// 產生失敗的鍵 → 失敗時間（損壞的檔案、還在寫入的檔案）
    failed: Mutex<HashMap<String, Instant>>,
}

impl Thumbnails {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, sem: Arc::new(Semaphore::new(CONCURRENCY)), inflight: Mutex::default(), failed: Mutex::default() }
    }

    /// 取得影片縮圖的檔案路徑；產生失敗回傳 None
    pub async fn get(&self, ffmpeg: &Path, video: &str) -> Option<PathBuf> {
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
        if self.failed.lock().unwrap().get(&key).is_some_and(|t| t.elapsed() < RETRY_FAILED) {
            return None;
        }
        let cell = self.inflight.lock().unwrap().entry(key.clone()).or_default().clone();
        let result = cell
            .get_or_init(|| async {
                let _permit = self.sem.clone().acquire_owned().await.ok()?;
                let _ = tokio::fs::create_dir_all(&self.dir).await;
                // 先取第 1 秒（避開開頭可能的黑畫面），太短的影片改取第一張；-threads 1 不和錄影搶 CPU
                for at in ["1", "0"] {
                    let out_s = out.display().to_string();
                    let args = ["-hide_banner", "-loglevel", "error", "-threads", "1", "-ss", at, "-i", video, "-frames:v", "1", "-vf", "scale=320:-2:flags=bilinear", "-y", &out_s];
                    let r = run(ffmpeg, &args, Duration::from_secs(20)).await;
                    if r.code == 0 && std::fs::metadata(&out).map(|m| m.len() > 0).unwrap_or(false) {
                        return Some(out.clone());
                    }
                }
                let _ = std::fs::remove_file(&out);
                let mut f = self.failed.lock().unwrap();
                if f.len() > 500 {
                    f.clear();
                }
                f.insert(key.clone(), Instant::now());
                None
            })
            .await
            .clone();
        self.inflight.lock().unwrap().remove(&key);
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(t.get(Path::new("false"), &video.display().to_string()).await, None);
        assert_eq!(t.failed.lock().unwrap().len(), 1);
        // 失敗後 10 分鐘內直接回傳 None（不會再執行）
        assert_eq!(t.get(Path::new("false"), &video.display().to_string()).await, None);
    }
}
