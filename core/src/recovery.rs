//! 救回意外中斷的錄影：錄影時每個分段每秒寫入一個 fragment（見 args::segment_args），
//! 程式當掉、停電或被強制結束時，儲存位置的 `.parts/<開始時間>/` 裡還留著分段。
//! 下次啟動時找出這些資料夾，用與正常停止相同的方式合併成一支錄影（或刪掉）。

use crate::paths::unique_path;
use crate::recorder::{cleanup_parts, merge_segments};
use crate::types::RecordingResult;
use crate::{tr, trf, Error, Result};
use std::path::{Path, PathBuf};

/// 一次沒有收尾的錄影
#[derive(Debug, Clone, PartialEq)]
pub struct Orphan {
    /// 分段資料夾（.parts/<開始時間>）
    pub dir: PathBuf,
    /// 開始時間（資料夾名稱，例如 2026-10-10_14-30-05）
    pub stamp: String,
    /// 有內容的分段（依順序）
    pub files: Vec<PathBuf>,
    /// 分段加起來的大小
    pub bytes: u64,
    /// 只錄聲音（資料夾裡有錄音卡片）
    pub audio_only: bool,
}

/// 儲存位置裡沒有收尾的錄影（最舊的在前）。active = 正在錄影的分段資料夾（不算）。
/// 沒有任何內容的資料夾直接刪掉
pub fn find(output_dir: &Path, active: Option<&Path>) -> Vec<Orphan> {
    let root = output_dir.join(".parts");
    let Ok(rd) = std::fs::read_dir(&root) else { return Vec::new() };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let dir = e.path();
        if !dir.is_dir() || active.is_some_and(|a| a == dir) {
            continue;
        }
        let mut files: Vec<(PathBuf, u64)> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|f| {
                        let p = f.path();
                        let name = p.file_name()?.to_str()?.to_string();
                        let len = f.metadata().ok()?.len();
                        (name.starts_with("seg_") && name.ends_with(".mp4") && len > 0).then_some((p, len))
                    })
                    .collect()
            })
            .unwrap_or_default();
        if files.is_empty() {
            cleanup_parts(Some(&dir));
            continue;
        }
        files.sort();
        out.push(Orphan {
            stamp: dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            audio_only: dir.join("card.png").exists(),
            bytes: files.iter().map(|f| f.1).sum(),
            files: files.into_iter().map(|f| f.0).collect(),
            dir,
        });
    }
    out.sort_by(|a, b| a.stamp.cmp(&b.stamp));
    out
}

/// 合併成一支錄影（檔名與正常停止時相同：Rec_<開始時間>.mp4），成功後刪掉分段
pub async fn recover(ffmpeg: &Path, o: &Orphan) -> Result<RecordingResult> {
    let output_dir = o.dir.parent().and_then(Path::parent).ok_or_else(|| Error::other(tr!("找不到儲存位置", "Save location not found")))?;
    let prefix = if o.audio_only { "錄音" } else { "Rec" };
    let out = unique_path(output_dir, &format!("{prefix}_{}", o.stamp), ".mp4");
    let files: Vec<String> = o.files.iter().map(|f| f.display().to_string()).collect();
    // 聲音有沒有錄：讀不到聲音結尾的分段就不截斷（merge_segments 會略過）
    let duration = merge_segments(ffmpeg, &files, &o.dir, &out, true).await.map_err(|detail| Error::other(trf!("無法合併分段：{detail}", "Couldn't merge the segments: {detail}")))?;
    cleanup_parts(Some(&o.dir));
    let out_str = out.display().to_string();
    Ok(RecordingResult {
        ok: true,
        path: Some(out_str.clone()),
        video_sec: duration.unwrap_or(0.0),
        bytes: std::fs::metadata(&out).map(|m| m.len()).ok(),
        message: trf!("已救回 {out_str}", "Recovered {out_str}"),
        ..Default::default()
    })
}

/// 不要了：刪掉分段
pub fn discard(o: &Orphan) {
    cleanup_parts(Some(&o.dir));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_leftover_segments() {
        let dir = tempfile::tempdir().unwrap();
        let parts = dir.path().join(".parts");
        let a = parts.join("2026-10-10_09-00-00");
        let b = parts.join("2026-10-11_10-00-00");
        let empty = parts.join("2026-10-11_11-00-00");
        let active = parts.join("2026-10-11_12-00-00");
        for d in [&a, &b, &empty, &active] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(a.join("seg_001.mp4"), b"22").unwrap();
        std::fs::write(a.join("seg_000.mp4"), b"1").unwrap();
        std::fs::write(a.join("concat.txt"), b"x").unwrap();
        std::fs::write(b.join("seg_000.mp4"), b"333").unwrap();
        std::fs::write(b.join("card.png"), b"png").unwrap();
        // 0 位元組的分段不算
        std::fs::write(empty.join("seg_000.mp4"), b"").unwrap();
        std::fs::write(active.join("seg_000.mp4"), b"4444").unwrap();

        let found = find(dir.path(), Some(&active));
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].stamp, "2026-10-10_09-00-00");
        assert_eq!(found[0].files.iter().map(|f| f.file_name().unwrap().to_str().unwrap()).collect::<Vec<_>>(), ["seg_000.mp4", "seg_001.mp4"]);
        assert_eq!((found[0].bytes, found[0].audio_only), (3, false));
        assert!(found[1].audio_only);
        // 沒有內容的資料夾刪掉；正在錄的不動
        assert!(!empty.exists());
        assert!(active.exists());
        // 沒有 .parts
        assert!(find(&dir.path().join("none"), None).is_empty());
        discard(&found[0]);
        assert!(!a.exists());
    }
}
