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
    find_at(output_dir, active, std::time::SystemTime::now())
}

/// 同上，now：現在時間（測試用）
fn find_at(output_dir: &Path, active: Option<&Path>, now: std::time::SystemTime) -> Vec<Orphan> {
    let root = output_dir.join(".parts");
    let Ok(rd) = std::fs::read_dir(&root) else { return Vec::new() };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let dir = e.path();
        if !dir.is_dir() || active.is_some_and(|a| a == dir) {
            continue;
        }
        // 讀不到（NAS 斷線、沒有權限）：不知道裡面有什麼，留著不動
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let entries: Vec<(PathBuf, std::fs::Metadata)> = rd.flatten().filter_map(|f| Some((f.path(), f.metadata().ok()?))).collect();
        // 另一台電腦（存到同一個 NAS 資料夾）的、最近還有寫入：它可能正在錄，不算、也不刪
        if from_other_pc(&dir) && (entries.iter().any(|(_, m)| recently_written(m, now)) || std::fs::metadata(&dir).is_ok_and(|m| recently_written(&m, now))) {
            continue;
        }
        let mut files: Vec<(PathBuf, u64)> = entries
            .into_iter()
            .filter_map(|(p, m)| {
                let name = p.file_name()?.to_str()?.to_string();
                (name.starts_with("seg_") && name.ends_with(".mp4") && m.len() > 0).then_some((p, m.len()))
            })
            .collect();
        if files.is_empty() {
            cleanup_parts(Some(&dir));
            continue;
        }
        // 依分段編號排（seg_1000 要排在 seg_999 後面，照字串排會跑到 seg_100 旁邊）
        files.sort_by_key(|(p, _)| (seg_number(p), p.clone()));
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
    let duration = merge_segments(ffmpeg, &files, &[], true, &o.dir, &out, true, None).await.map_err(|detail| Error::other(trf!("無法合併分段：{detail}", "Couldn't merge the segments: {detail}")))?;
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

/// 分段資料夾裡記下是哪台電腦錄的（見 from_other_pc）
const OWNER_FILE: &str = "owner.txt";

fn this_pc() -> String {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default()
}

/// 開始錄影時記下這個分段資料夾是哪台電腦的
pub(crate) fn mark_owner(dir: &Path) {
    let _ = std::fs::write(dir.join(OWNER_FILE), this_pc());
}

/// 別台電腦錄的（沒有記錄的是舊版本留下的，當作這台的）
fn from_other_pc(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join(OWNER_FILE)).is_ok_and(|o| !o.trim().is_empty() && !o.trim().eq_ignore_ascii_case(this_pc().trim()))
}

/// 別台電腦的分段資料夾這麼久以內有寫入，當作還在錄（錄影中每秒都會寫入）
const LIVE_WINDOW: std::time::Duration = std::time::Duration::from_secs(120);

fn recently_written(m: &std::fs::Metadata, now: std::time::SystemTime) -> bool {
    // 修改時間比現在還晚（時鐘不一致的 NAS）也當作還在錄
    m.modified().is_ok_and(|t| now.duration_since(t).map_or(true, |age| age < LIVE_WINDOW))
}

/// seg_012.mp4 → 12（看不懂的排最後）
fn seg_number(p: &Path) -> u64 {
    p.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_prefix("seg_")?.strip_suffix(".mp4")?.parse().ok()).unwrap_or(u64::MAX)
}

/// 不要了：刪掉沒有存好的錄影片段：移到資源回收筒（按錯了還能還原）；移不進去時留著不刪，回傳原因。
/// 網路資料夾（NAS）沒有資源回收筒，Windows 會直接刪掉
pub fn discard(o: &Orphan) -> std::result::Result<(), String> {
    if cfg!(windows) {
        crate::recycle::move_to_recycle_bin(&[o.dir.display().to_string()]).map_err(|e| e.message().to_string())?;
        // 只有空資料夾才刪得掉（.parts 裡還有其他片段時留著）
        if let Some(parent) = o.dir.parent() {
            let _ = std::fs::remove_dir(parent);
        }
        Ok(())
    } else {
        cleanup_parts(Some(&o.dir));
        Ok(())
    }
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
        // 當掉時留下的重播緩衝（沒存下的暫存檔）：不救回，直接刪掉
        let replay = parts.join("2026-10-11_13-00-00");
        for d in [&a, &b, &empty, &active, &replay] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(a.join("seg_001.mp4"), b"22").unwrap();
        // 超過 999 段：照編號排在最後（照字串排會排到 seg_100 旁邊）
        std::fs::write(a.join("seg_1000.mp4"), b"7").unwrap();
        std::fs::write(a.join("seg_100.mp4"), b"6").unwrap();
        std::fs::write(a.join("seg_000.mp4"), b"1").unwrap();
        std::fs::write(a.join("concat.txt"), b"x").unwrap();
        std::fs::write(b.join("seg_000.mp4"), b"333").unwrap();
        std::fs::write(b.join("card.png"), b"png").unwrap();
        // 0 位元組的分段不算
        std::fs::write(empty.join("seg_000.mp4"), b"").unwrap();
        std::fs::write(active.join("seg_000.mp4"), b"4444").unwrap();
        std::fs::write(replay.join("rp_000_00003.mp4"), b"55555").unwrap();

        // 另一台電腦（同一個 NAS 資料夾）正在錄的：剛寫入時不算、也不刪；一小時沒動才當作沒收尾
        let other = parts.join("2026-10-11_14-00-00");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join(OWNER_FILE), "ANOTHER-PC-XYZ").unwrap();
        std::fs::write(other.join("seg_000.mp4"), b"666666").unwrap();
        let found = find(dir.path(), Some(&active));
        assert_eq!(found.len(), 2, "這台電腦的照樣找得到（剛當掉、馬上救回）");
        assert!(other.exists());
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
        assert_eq!(find_at(dir.path(), Some(&active), later).len(), 3);
        std::fs::remove_dir_all(&other).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].stamp, "2026-10-10_09-00-00");
        assert_eq!(found[0].files.iter().map(|f| f.file_name().unwrap().to_str().unwrap()).collect::<Vec<_>>(), ["seg_000.mp4", "seg_001.mp4", "seg_100.mp4", "seg_1000.mp4"]);
        assert_eq!((found[0].bytes, found[0].audio_only), (5, false));
        assert!(found[1].audio_only);
        // 沒有內容的資料夾刪掉；正在錄的不動
        assert!(!empty.exists() && !replay.exists());
        assert!(active.exists());
        // 沒有 .parts
        assert!(find(&dir.path().join("none"), None).is_empty());
        discard(&found[0]).unwrap();
        assert!(!a.exists());
    }
}
