//! 重播緩衝：在背景一直錄，只保留最近幾分鐘；發生想留下的事（遊戲精彩畫面、程式突然出錯）時「存下最近 N 分鐘」。
//!
//! 錄影器用 FFmpeg 的 segment 輸出，每 10 秒換一個檔案（`.parts/<開始時間>/rp_<第幾次啟動>_<第幾個>.mp4`，
//! 每秒寫入一次），定期刪掉太舊的；存下時把最近的幾個檔案接起來（不重新壓縮，一兩秒就好），存成 `Replay_<時間>.mp4`。
//! 停止時緩衝全部刪掉。不能暫停、不能加標記；倒數、最長錄影時間不適用。

use crate::app::App;
use crate::args::REPLAY_CHUNK_SEC;
use crate::types::{RecordConfig, RecordingResult};
use crate::{tr, trf, Error, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// 可選的長度（秒）
pub const LENGTHS: [u32; 5] = [30, 60, 120, 300, 600];
pub const DEFAULT_SEC: u32 = 120;
/// 操作視窗設定（ui）裡的欄位
pub const UI_KEY: &str = "replaySec";
/// 剛開始寫、還沒有畫面的檔案（只有檔頭）
const MIN_CHUNK_BYTES: u64 = 4096;
/// 比保留的長度再多留這麼久才刪（存下時正在讀的檔案不會被刪掉）
const PRUNE_SLACK_SEC: u64 = 30;

/// 操作視窗設定的長度（不在可選範圍內時用預設）
pub fn sec_from_ui(ui: Option<&Value>) -> u32 {
    ui.and_then(|u| u.get(UI_KEY)).and_then(Value::as_u64).map(|v| v as u32).filter(|v| LENGTHS.contains(v)).unwrap_or(DEFAULT_SEC)
}

/// 「30 秒」「2 分鐘」
pub fn length_label(sec: u32) -> String {
    if sec < 60 {
        trf!("{sec} 秒", "{sec} s")
    } else {
        let m = sec / 60;
        if crate::i18n::is_en() {
            format!("{m} min")
        } else {
            format!("{m} 分鐘")
        }
    }
}

/// 緩衝裡的一個檔案
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub path: PathBuf,
    /// 最後寫入的時間（也就是這個檔案錄到哪裡）
    pub modified: SystemTime,
    pub len: u64,
}

/// 緩衝裡的檔案，依錄的順序（檔名：第幾次啟動 FFmpeg、第幾個檔案，都補零）
pub fn chunks(dir: &Path) -> Vec<Chunk> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<Chunk> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if !(name.starts_with("rp_") && name.ends_with(".mp4")) {
                return None;
            }
            let m = e.metadata().ok()?;
            Some(Chunk { path: e.path(), modified: m.modified().ok()?, len: m.len() })
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// 存下最近 sec 秒要用的檔案：最後寫入的時間在這段時間內（第一個可能從更早開始，最多多 REPLAY_CHUNK_SEC 秒）；
/// 剛開始寫、還沒有畫面的不算
pub fn pick(chunks: &[Chunk], now: SystemTime, sec: u32) -> Vec<PathBuf> {
    let from = now.checked_sub(Duration::from_secs(sec as u64)).unwrap_or(SystemTime::UNIX_EPOCH);
    chunks.iter().filter(|c| c.modified >= from && c.len >= MIN_CHUNK_BYTES).map(|c| c.path.clone()).collect()
}

/// 可以刪掉的：最後寫入的時間比保留的長度再早 30 秒以上
pub fn stale(chunks: &[Chunk], now: SystemTime, sec: u32) -> Vec<PathBuf> {
    let keep = Duration::from_secs(sec as u64 + REPLAY_CHUNK_SEC as u64 + PRUNE_SLACK_SEC);
    chunks.iter().filter(|c| now.duration_since(c.modified).is_ok_and(|d| d > keep)).map(|c| c.path.clone()).collect()
}

impl App {
    /// 重播緩衝中
    pub fn replay_active(&self) -> bool {
        self.recorder.status().replay.is_some()
    }

    /// 開始重播緩衝；config.replay_sec 是 0 時用操作視窗設定的長度
    pub async fn replay_start(&self, config: RecordConfig) -> Result<()> {
        let sec = if config.replay_sec > 0 { config.replay_sec } else { sec_from_ui(self.settings.load().ui.as_ref()) };
        crate::actions::record_start(self, RecordConfig { replay_sec: sec, ..config }).await
    }

    /// 存下最近幾分鐘；notify = 用系統匣通知結果（快速鍵、系統匣選單）
    pub async fn replay_save(self: &Arc<Self>, notify: bool) -> Result<RecordingResult> {
        let r = self.recorder.save_replay().await;
        if notify {
            match &r {
                Ok(res) => {
                    let name = res.path.as_deref().map(|p| Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).unwrap_or_default();
                    self.notify(tr!("已存下重播", "Replay saved"), &trf!("{name}（{}）", "{name} ({})", crate::format::video_clock(res.video_sec)), false);
                }
                Err(e) => self.notify(tr!("無法存下重播", "Couldn't save the replay"), e.message(), true),
            }
        }
        r
    }

    /// 重播緩衝的快速鍵：沒在錄影時開始，重播緩衝中存下；一般錄影中不動作（提示）
    pub async fn replay_hotkey(self: &Arc<Self>, config: RecordConfig) {
        if self.replay_active() {
            let _ = self.replay_save(true).await;
        } else if self.recorder.active() || self.recorder.starting() {
            self.notify(tr!("重播緩衝", "Replay buffer"), tr!("正在錄影，停止後才能開始重播緩衝", "Recording is in progress. Stop it before starting the replay buffer."), true);
        } else {
            match self.replay_start(config).await {
                Ok(()) => {
                    let sec = sec_from_ui(self.settings.load().ui.as_ref());
                    self.notify(tr!("重播緩衝已開始", "Replay buffer started"), &trf!("再按一次快速鍵就存下最近 {}", "Press the shortcut again to save the last {}", length_label(sec)), false);
                }
                Err(e) => self.notify(tr!("無法開始重播緩衝", "Couldn't start the replay buffer"), e.message(), true),
            }
        }
    }
}

/// 緩衝裡還沒有畫面（錄影器用）
pub(crate) fn no_frames() -> Error {
    Error::config(tr!("緩衝裡還沒有畫面，請稍等幾秒再存", "The buffer has no video yet. Wait a few seconds and try again."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn chunk(name: &str, ago: u64, len: u64, now: SystemTime) -> Chunk {
        Chunk { path: PathBuf::from(name), modified: now - Duration::from_secs(ago), len }
    }

    #[test]
    fn picks_recent_files_and_prunes_old_ones() {
        let now = SystemTime::now();
        let list = vec![
            chunk("rp_000_00000.mp4", 200, 900_000, now),
            chunk("rp_000_00001.mp4", 190, 900_000, now),
            chunk("rp_000_00002.mp4", 125, 900_000, now),
            chunk("rp_000_00003.mp4", 115, 900_000, now),
            // 移動範圍後重新啟動 FFmpeg
            chunk("rp_001_00000.mp4", 5, 900_000, now),
            // 剛開始寫：只有檔頭
            chunk("rp_001_00001.mp4", 0, 1_200, now),
        ];
        // 最近 2 分鐘：最後寫入在 120 秒內的（00003 從 125 秒前開始，多一點沒關係）
        let names = |v: Vec<PathBuf>| v.iter().map(|p| p.display().to_string()).collect::<Vec<_>>();
        assert_eq!(names(pick(&list, now, 120)), ["rp_000_00003.mp4", "rp_001_00000.mp4"]);
        assert_eq!(names(pick(&list, now, 30)), ["rp_001_00000.mp4"]);
        // 太舊的才刪（2 分鐘 + 10 秒 + 30 秒）
        assert_eq!(names(stale(&list, now, 120)), ["rp_000_00000.mp4", "rp_000_00001.mp4"]);
        assert!(stale(&list, now, 600).is_empty());
    }

    #[test]
    fn lists_chunks_in_recording_order() {
        let dir = tempfile::tempdir().unwrap();
        for n in ["rp_001_00000.mp4", "rp_000_00010.mp4", "rp_000_00002.mp4", "card.png", "concat.txt"] {
            std::fs::write(dir.path().join(n), b"x").unwrap();
        }
        let got: Vec<String> = chunks(dir.path()).iter().map(|c| c.path.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(got, ["rp_000_00002.mp4", "rp_000_00010.mp4", "rp_001_00000.mp4"]);
    }

    #[test]
    fn length_setting() {
        assert_eq!(sec_from_ui(None), DEFAULT_SEC);
        assert_eq!(sec_from_ui(Some(&json!({ "replaySec": 300 }))), 300);
        assert_eq!(sec_from_ui(Some(&json!({ "replaySec": 7 }))), DEFAULT_SEC);
        assert_eq!(length_label(30), "30 秒");
        assert_eq!(length_label(120), "2 分鐘");
    }
}
