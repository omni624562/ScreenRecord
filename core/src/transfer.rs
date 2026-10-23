//! 傳到 NAS（媒體庫資料夾）：
//! - 先寫成隱藏的暫存檔（.檔名.part），傳完才改成正式檔名：網路中斷不會在媒體庫留下半支壞檔，
//!   Jellyfin 的即時監控也不會掃到還沒傳完的檔案
//! - 傳的時候記下進度（畫面顯示「1.2 / 3.4 GB」），可以取消
//! - 連不到 NAS 時記進待傳清單（存在設定資料夾的 pending-sends.json），連上後補傳
//! - 從 Jellyfin 下載到這台電腦也用同樣的方式（先寫暫存檔、顯示進度、可以取消）
//! - 傳好之後 Jellyfin 還在整理的（掃描、時間軸預覽縮圖）也列出來，好了才拿掉

use crate::{tr, trf};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// 一個正在傳的檔案
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Active {
    pub id: u64,
    pub name: String,
    pub done: u64,
    pub total: u64,
    /// 從 Jellyfin 下載（不是傳到 NAS）
    pub down: bool,
}

struct Slot {
    id: u64,
    name: String,
    done: Arc<AtomicU64>,
    total: u64,
    cancel: Arc<AtomicBool>,
    down: bool,
}

static ACTIVE: Mutex<Vec<Slot>> = Mutex::new(Vec::new());
static NEXT: AtomicU64 = AtomicU64::new(1);

/// 正在傳的檔案（畫面顯示進度用）；只列大於 8 MB 的（字幕、小檔一下就傳完，不用顯示）
pub fn active() -> Vec<Active> {
    ACTIVE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|s| s.total > 8 << 20)
        .map(|s| Active { id: s.id, name: s.name.clone(), done: s.done.load(Ordering::Relaxed), total: s.total, down: s.down })
        .collect()
}

/// 取消正在傳的那一個（暫存檔會刪掉，原本 NAS 上的那一份不動）
pub fn cancel(id: u64) {
    if let Some(s) = ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|s| s.id == id) {
        s.cancel.store(true, Ordering::Relaxed);
    }
}

/// 取消時 copy 回傳的錯誤
pub fn is_cancelled(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::Interrupted
}

/// 複製到 NAS：先寫暫存檔，傳完才換成正式檔名（同名的舊檔這時才被換掉），修改時間和原檔一樣。回傳複製了幾個位元組
pub fn copy(from: &Path, to: &Path) -> std::io::Result<u64> {
    let total = std::fs::metadata(from)?.len();
    let src = std::fs::File::open(from)?;
    let r = save(src, total, to, false);
    if r.is_ok() {
        if let Ok(t) = std::fs::metadata(from).and_then(|m| m.modified()) {
            crate::paths::set_mtime(to, t);
        }
    }
    r
}

/// 從 src 讀出來存成 to（Jellyfin 下載也用）：先寫暫存檔，完整了才換成正式檔名。total：全部幾個位元組（不知道時 0）
pub fn save(src: impl Read, total: u64, to: &Path, down: bool) -> std::io::Result<u64> {
    let name = to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = to.with_file_name(format!(".{name}.part"));
    let (id, done, cancel) = (NEXT.fetch_add(1, Ordering::Relaxed), Arc::new(AtomicU64::new(0)), Arc::new(AtomicBool::new(false)));
    ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).push(Slot { id, name: name.clone(), done: done.clone(), total, cancel: cancel.clone(), down });
    let r = copy_to_tmp(src, &tmp, &done, &cancel).and_then(|n| {
        // 少了一截（下載到一半斷線）：不算完成
        if total > 0 && n < total {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, tr!("連線中斷，沒有收完", "The connection dropped before it finished")));
        }
        // Windows 的 rename 遇到同名檔會換掉；有的 NAS（SMB）不行：先把舊的改名留著，換好了才刪，
        // 換不過去時把舊的改回來（不會新舊兩份都不見）
        if std::fs::rename(&tmp, to).is_err() {
            let old = to.with_file_name(format!(".{name}.old"));
            let moved = to.exists() && std::fs::rename(to, &old).is_ok();
            if let Err(e) = std::fs::rename(&tmp, to) {
                if moved {
                    let _ = std::fs::rename(&old, to);
                }
                return Err(e);
            }
            if moved {
                let _ = std::fs::remove_file(&old);
            }
        }
        Ok(n)
    });
    ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).retain(|s| s.id != id);
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

fn copy_to_tmp(mut src: impl Read, tmp: &Path, done: &AtomicU64, cancel: &AtomicBool) -> std::io::Result<u64> {
    let mut dst = std::io::BufWriter::with_capacity(4 << 20, std::fs::File::create(tmp)?);
    let mut buf = vec![0u8; 4 << 20];
    let mut n = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, tr!("已取消", "Cancelled")));
        }
        let k = src.read(&mut buf)?;
        if k == 0 {
            break;
        }
        dst.write_all(&buf[..k])?;
        n += k as u64;
        done.store(n, Ordering::Relaxed);
    }
    dst.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    Ok(n)
}

/// Jellyfin 還在整理的影片：（檔名, 在做什麼）
static STAGES: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// 記下（或拿掉）這支影片 Jellyfin 正在做什麼（畫面右下角顯示）
pub fn set_stage(name: &str, what: Option<String>) {
    let mut g = STAGES.lock().unwrap_or_else(|e| e.into_inner());
    g.retain(|(n, _)| n != name);
    if let Some(w) = what {
        g.push((name.to_string(), w));
    }
}

pub fn stages() -> Vec<(String, String)> {
    STAGES.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// 連得到媒體庫資料夾嗎（NAS 沒開、不在家時連不到）
pub fn reachable(root: &str) -> bool {
    let root = root.trim();
    !root.is_empty() && Path::new(root).is_dir()
}

/// 等待傳到 NAS 的一個檔案：本機檔案、要放進媒體庫的哪個資料夾（family/2024；空的 = 最上層）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub file: String,
    #[serde(default)]
    pub sub: String,
}

static PENDING: Mutex<Option<(Option<PathBuf>, Vec<Pending>)>> = Mutex::new(None);

/// 讀出待傳清單（程式啟動時；已經讀過就不再讀）
pub fn init_pending(file: PathBuf) {
    let mut g = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    if g.as_ref().is_some_and(|(f, _)| f.is_some()) {
        return;
    }
    let mut list: Vec<Pending> = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    list.extend(g.take().map(|(_, l)| l).unwrap_or_default());
    *g = Some((Some(file), list));
}

fn with_pending<T>(f: impl FnOnce(&mut Vec<Pending>) -> (T, bool)) -> T {
    let mut g = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    let (file, list) = g.get_or_insert_with(|| (None, vec![]));
    let (out, changed) = f(list);
    if changed {
        if let Some(file) = file.as_ref() {
            if let Err(e) = std::fs::write(file, serde_json::to_string(list).unwrap_or_default()) {
                crate::warn!("[傳到資料夾] 記下待傳清單失敗：{e}");
            }
        }
    }
    out
}

/// 記進待傳清單（同一個檔案只記一次，換成新的資料夾）
pub fn add_pending(file: &Path, sub: &str) {
    let file = file.display().to_string();
    crate::info!("[傳到資料夾] 連不到 NAS，先記下等一下補傳：{file}");
    with_pending(|list| {
        list.retain(|p| p.file != file);
        list.push(Pending { file, sub: sub.to_string() });
        ((), true)
    })
}

pub fn remove_pending(file: &str) {
    with_pending(|list| {
        let n = list.len();
        list.retain(|p| p.file != file);
        ((), list.len() != n)
    })
}

pub fn pending() -> Vec<Pending> {
    with_pending(|list| (list.clone(), false))
}

pub fn is_pending(file: &str) -> bool {
    with_pending(|list| (list.iter().any(|p| p.file == file), false))
}

/// 本機檔案改名後，待傳清單跟著換
pub fn rename_pending(old: &str, new: &str) {
    with_pending(|list| {
        let mut changed = false;
        for p in list.iter_mut().filter(|p| p.file == old) {
            p.file = new.to_string();
            changed = true;
        }
        ((), changed)
    })
}

/// 待傳清單的說明（檔案庫標籤的提示）
pub fn pending_tip(sub: &str) -> String {
    let place = if sub.is_empty() { tr!("Jellyfin", "Jellyfin").to_string() } else { sub.replace('/', " › ") };
    trf!("連不到 Jellyfin 的資料夾，連上後會自動傳到「{place}」", "The Jellyfin folder isn't reachable. It will be sent to “{place}” once it is")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_through_a_hidden_part_file_and_keeps_the_time() {
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (dir.path().join("a.mp4"), dir.path().join("nas").join("a.mp4"));
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::write(&from, vec![7u8; 100]).unwrap();
        let t = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        crate::paths::set_mtime(&from, t);
        // 舊版本被換掉
        std::fs::write(&to, "old").unwrap();
        assert_eq!(copy(&from, &to).unwrap(), 100);
        assert_eq!(std::fs::read(&to).unwrap(), vec![7u8; 100]);
        assert_eq!(std::fs::metadata(&to).unwrap().modified().unwrap(), t);
        assert!(!to.with_file_name(".a.mp4.part").exists());
        assert!(active().is_empty());
        // 來源不見：不留暫存檔、舊檔不動
        assert!(copy(&dir.path().join("gone.mp4"), &to).is_err());
        assert_eq!(std::fs::read(&to).unwrap().len(), 100);
    }

    #[test]
    fn cancelling_leaves_the_old_copy() {
        let dir = tempfile::tempdir().unwrap();
        let tmp = dir.path().join(".b.part");
        std::fs::write(dir.path().join("b"), "x").unwrap();
        let e = copy_to_tmp(std::fs::File::open(dir.path().join("b")).unwrap(), &tmp, &AtomicU64::new(0), &AtomicBool::new(true)).unwrap_err();
        assert!(is_cancelled(&e));
    }

    #[test]
    fn a_short_download_is_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let to = dir.path().join("c.mp4");
        assert!(save(&b"12345"[..], 10, &to, true).is_err());
        assert!(!to.exists() && !dir.path().join(".c.mp4.part").exists());
        assert_eq!(save(&b"12345"[..], 5, &to, true).unwrap(), 5);
        assert_eq!(std::fs::read(&to).unwrap(), b"12345");
        set_stage("c.mp4", Some("掃描中".into()));
        assert_eq!(stages(), [("c.mp4".to_string(), "掃描中".to_string())]);
        set_stage("c.mp4", None);
        assert!(stages().is_empty());
    }

    #[test]
    fn pending_sends_are_remembered() {
        let f = Path::new("/rec/待傳測試.mp4");
        add_pending(f, "family");
        add_pending(f, "family/2024");
        let key = f.display().to_string();
        assert!(is_pending(&key));
        assert_eq!(pending().iter().filter(|p| p.file == key).map(|p| p.sub.as_str()).collect::<Vec<_>>(), ["family/2024"]);
        rename_pending(&key, "/rec/新名.mp4");
        assert!(!is_pending(&key) && is_pending("/rec/新名.mp4"));
        remove_pending("/rec/新名.mp4");
        assert!(!is_pending("/rec/新名.mp4"));
        assert!(!reachable("") && reachable(&std::env::temp_dir().display().to_string()));
    }
}
