//! 自動匯入資料夾：定時看設定的資料夾（例如 OBS 的輸出資料夾、手機同步資料夾、NAS），有新影片時匯入到儲存位置
//! （MOV、MKV… 轉成 MP4，見 actions::import_videos），可以接著做「錄完自動處理」。
//!
//! - 第一次看到一個資料夾時，裡面原本就有的影片不匯入（只匯入之後新增的）
//! - 檔案還在寫入（大小、修改時間還在變，或剛改過不到 5 秒）時先等
//! - 匯入過的記在資料夾 watch_seen.json（路徑 + 大小 + 修改時間），重新開啟程式也不會重複匯入
//! - 錄影中、有轉檔工作時先不做

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// 多久看一次（秒）
pub const POLL_SEC: u64 = 10;
/// 修改時間至少過了多久才算寫完（毫秒）
const SETTLE_MS: f64 = 5_000.0;
/// 記住的檔案上限（太多時丟掉最舊的一半）
const SEEN_MAX: usize = 20_000;
/// 匯入失敗幾次後就不再試（損壞的檔案）
const MAX_TRIES: u32 = 3;

/// 設定（操作視窗的 ui 設定）：要看的資料夾
pub fn dirs_from_ui(ui: Option<&Value>) -> Vec<PathBuf> {
    ui.and_then(|u| u.get("watchDirs"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|d| d.as_str()).map(str::trim).filter(|d| !d.is_empty()).map(PathBuf::from).collect())
        .unwrap_or_default()
}

/// 設定：匯入後接著做「錄完自動處理」
pub fn after_from_ui(ui: Option<&Value>) -> bool {
    ui.and_then(|u| u.get("watchAfter")).and_then(|v| v.as_bool()).unwrap_or(false)
}

/// dir 是不是儲存位置本身或它底下的資料夾（不能看：匯入的檔案會再被看到）
pub fn inside(dir: &Path, root: &Path) -> bool {
    let norm = |p: &Path| p.display().to_string().replace('\\', "/").trim_end_matches('/').to_lowercase();
    let (d, r) = (norm(dir), norm(root));
    !r.is_empty() && (d == r || d.starts_with(&format!("{r}/")))
}

fn key(path: &str, bytes: u64, mtime: f64) -> String {
    format!("{}|{bytes}|{}", path.to_lowercase(), mtime as u64)
}

/// 一個檔案的狀態（看的時候讀到的）
#[derive(Debug, Clone, PartialEq)]
pub struct Seen {
    pub path: String,
    pub bytes: u64,
    pub mtime: f64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    dirs: Vec<String>,
    #[serde(default)]
    seen: Vec<String>,
}

/// 看資料夾的狀態
#[derive(Debug, Default)]
pub struct Watcher {
    /// 已經看過的資料夾（第一次看到時原本的檔案不匯入）
    dirs: HashSet<String>,
    /// 匯入過（或原本就在）的檔案
    seen: HashSet<String>,
    seen_order: Vec<String>,
    /// 上一次看到的大小與修改時間（兩次一樣才算寫完）
    last: HashMap<String, (u64, u64)>,
    tries: HashMap<String, u32>,
    file: Option<PathBuf>,
}

impl Watcher {
    /// 從檔案讀回（file = None：不存檔，測試用）
    pub fn load(file: Option<PathBuf>) -> Watcher {
        let saved: Saved = file.as_ref().and_then(|f| std::fs::read(f).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Watcher { dirs: saved.dirs.into_iter().collect(), seen: saved.seen.iter().cloned().collect(), seen_order: saved.seen, file, ..Default::default() }
    }

    fn save(&self) {
        let Some(f) = &self.file else { return };
        let saved = Saved { dirs: self.dirs.iter().cloned().collect(), seen: self.seen_order.clone() };
        if let Ok(b) = serde_json::to_vec(&saved) {
            let tmp = f.with_extension("tmp");
            if std::fs::write(&tmp, b).is_ok() {
                let _ = std::fs::rename(&tmp, f);
            }
        }
    }

    fn remember(&mut self, k: String) {
        if self.seen.insert(k.clone()) {
            self.seen_order.push(k);
        }
        if self.seen_order.len() > SEEN_MAX {
            let drop: Vec<String> = self.seen_order.drain(..SEEN_MAX / 2).collect();
            for d in drop {
                self.seen.remove(&d);
            }
        }
    }

    /// 這次看到的檔案（每個資料夾裡可以匯入的影片）→ 要匯入的（已經寫完、還沒匯入過）。
    /// 第一次看到的資料夾：裡面原本的檔案都記成看過，不匯入
    pub fn ready(&mut self, found: &[(PathBuf, Vec<Seen>)], now_ms: f64) -> Vec<String> {
        let mut out = vec![];
        let mut changed = false;
        for (dir, files) in found {
            let d = dir.display().to_string().to_lowercase();
            if !self.dirs.contains(&d) {
                for f in files {
                    self.remember(key(&f.path, f.bytes, f.mtime));
                }
                self.dirs.insert(d);
                changed = true;
                continue;
            }
            for f in files {
                let k = key(&f.path, f.bytes, f.mtime);
                if self.seen.contains(&k) {
                    continue;
                }
                let now = (f.bytes, f.mtime as u64);
                let stable = self.last.insert(f.path.to_lowercase(), now) == Some(now);
                if stable && now_ms - f.mtime >= SETTLE_MS && f.bytes > 0 {
                    out.push(f.path.clone());
                }
            }
        }
        if changed {
            self.save();
        }
        out
    }

    /// 匯入完（ok、failed 是原檔路徑）：成功的記下來；失敗的再試幾次，還是不行就不再試；
    /// 都不在裡面的（開始錄影、中途停止）下次再匯入
    pub fn finished(&mut self, files: &[Seen], ok: &[String], failed: &[String]) {
        let has = |list: &[String], p: &str| list.iter().any(|x| x.eq_ignore_ascii_case(p));
        for f in files {
            let k = key(&f.path, f.bytes, f.mtime);
            let done = has(ok, &f.path)
                || (has(failed, &f.path) && {
                    let tries = self.tries.entry(k.clone()).or_default();
                    *tries += 1;
                    *tries >= MAX_TRIES
                });
            if done {
                self.tries.remove(&k);
                self.remember(k);
            }
        }
        self.save();
    }
}

/// 列出資料夾裡（不含子資料夾）可以匯入的影片
pub fn list(dir: &Path) -> Vec<Seen> {
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    rd.flatten()
        .filter(|e| crate::import::is_importable(&e.file_name().to_string_lossy()))
        .filter_map(|e| {
            let m = e.metadata().ok().filter(|m| m.is_file())?;
            Some(Seen { path: e.path().display().to_string(), bytes: m.len(), mtime: crate::paths::mtime_ms(&m) })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(p: &str, bytes: u64, mtime: f64) -> Seen {
        Seen { path: p.into(), bytes, mtime }
    }

    #[test]
    fn existing_files_are_skipped_and_new_ones_wait_until_written() {
        let mut w = Watcher::load(None);
        let dir = PathBuf::from("/obs");
        // 第一次看到資料夾：原本的影片不匯入
        assert!(w.ready(&[(dir.clone(), vec![f("/obs/old.mkv", 100, 0.0)])], 100_000.0).is_empty());
        // 新的檔案：第一次看到先記下大小，第二次一樣、而且改過超過 5 秒才匯入
        let new = f("/obs/new.mkv", 500, 100_000.0);
        let list = vec![f("/obs/old.mkv", 100, 0.0), new.clone()];
        assert!(w.ready(&[(dir.clone(), list.clone())], 101_000.0).is_empty());
        assert!(w.ready(&[(dir.clone(), list.clone())], 103_000.0).is_empty(), "剛改過不到 5 秒");
        assert_eq!(w.ready(&[(dir.clone(), list.clone())], 106_000.0), ["/obs/new.mkv"]);
        // 還在寫入（變大了）：重新等
        let growing = vec![f("/obs/new.mkv", 900, 107_000.0)];
        assert!(w.ready(&[(dir.clone(), growing.clone())], 120_000.0).is_empty());
        assert_eq!(w.ready(&[(dir.clone(), growing.clone())], 120_000.0), ["/obs/new.mkv"]);
        // 匯入成功：不再匯入
        w.finished(&growing, &["/obs/new.mkv".into()], &[]);
        assert!(w.ready(&[(dir.clone(), growing.clone())], 130_000.0).is_empty());
    }

    #[test]
    fn failures_are_retried_a_few_times_and_state_is_saved() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("watch_seen.json");
        let dir = PathBuf::from("/nas");
        let bad = vec![f("/nas/bad.avi", 10, 0.0)];
        let mut w = Watcher::load(Some(file.clone()));
        w.ready(&[(dir.clone(), vec![])], 50_000.0);
        w.ready(&[(dir.clone(), bad.clone())], 50_000.0);
        assert_eq!(w.ready(&[(dir.clone(), bad.clone())], 50_000.0).len(), 1);
        w.finished(&bad, &[], &[]);
        assert_eq!(w.ready(&[(dir.clone(), bad.clone())], 50_000.0).len(), 1, "沒做到（開始錄影）：下次再匯入，不算失敗");
        let why = ["/nas/bad.avi".to_string()];
        for _ in 0..2 {
            w.finished(&bad, &[], &why);
            assert_eq!(w.ready(&[(dir.clone(), bad.clone())], 50_000.0).len(), 1, "失敗了再試");
        }
        w.finished(&bad, &[], &why);
        assert!(w.ready(&[(dir.clone(), bad.clone())], 50_000.0).is_empty(), "試了三次就不再試");
        // 重新開啟程式：記得看過的資料夾與檔案
        let mut again = Watcher::load(Some(file));
        assert!(again.ready(&[(dir.clone(), bad.clone())], 60_000.0).is_empty());
        let fresh = vec![f("/nas/fresh.mp4", 10, 0.0)];
        again.ready(&[(dir.clone(), fresh.clone())], 60_000.0);
        assert_eq!(again.ready(&[(dir, fresh)], 60_000.0), ["/nas/fresh.mp4"], "已經看過的資料夾：新檔案照常匯入");
    }

    #[test]
    fn settings_and_inside() {
        let ui = serde_json::json!({ "watchDirs": ["D:\\OBS", " ", "\\\\nas\\video"], "watchAfter": true });
        assert_eq!(dirs_from_ui(Some(&ui)), [PathBuf::from("D:\\OBS"), PathBuf::from("\\\\nas\\video")]);
        assert!(after_from_ui(Some(&ui)) && !after_from_ui(None));
        assert!(inside(Path::new("C:\\Videos\\Timelapse\\sub"), Path::new("C:\\Videos\\Timelapse")));
        assert!(inside(Path::new("c:/videos/timelapse/"), Path::new("C:\\Videos\\Timelapse")));
        assert!(!inside(Path::new("C:\\Videos\\Timelapse2"), Path::new("C:\\Videos\\Timelapse")));
    }
}
