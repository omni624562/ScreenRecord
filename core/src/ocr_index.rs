//! 截圖的文字索引：在背景用 Windows 內建的文字辨識把每張截圖裡的字讀出來存著，
//! 「全部截圖」的搜尋就能找圖裡的字（例如「502」找到那張 502 Bad Gateway）。
//!
//! 存在 ocr_index.json：小寫路徑 → (大小, 修改時間, 字)。檔案改過（大小或時間不同）就重新辨識。
//! 一次只辨識一張，在背景慢慢做；辨識過的不再做。不支援文字辨識的平台（或沒有可用的語言）就停下來不再試。

use crate::paths::mtime_ms;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Entry {
    bytes: u64,
    mtime: f64,
    /// 正規化過的字（小寫、去掉空白）
    text: String,
    /// 小寫、保留字與字之間一個空白（找常見詞時分得出英文單字）
    #[serde(default)]
    words: String,
    /// 格式版本：舊版沒有 words，要重新辨識一次
    #[serde(default)]
    v: u8,
}

/// 目前的格式版本
const VERSION: u8 = 2;

pub struct OcrIndex {
    file: PathBuf,
    map: Mutex<HashMap<String, Entry>>,
    running: AtomicBool,
    /// 這一輪要辨識幾張、做完幾張（介面顯示進度）
    total: AtomicUsize,
    done: AtomicUsize,
    /// 不能辨識（不支援、沒有語言）：這次執行不再試
    unavailable: AtomicBool,
    /// 內容改過幾次（常見詞等統計用它判斷要不要重算）
    generation: std::sync::atomic::AtomicU64,
}

/// 小寫、連續的空白變成一個
pub fn words(s: &str) -> String {
    s.split_whitespace().map(|w| w.to_lowercase()).collect::<Vec<_>>().join(" ")
}

/// 辨識常認錯的字當成一樣：o 與 0、l / i / | 與 1（只用在比對）
pub fn fold(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'o' => '0',
            'l' | 'i' | '|' => '1',
            c => c,
        })
        .collect()
}

/// 搜尋與存的字都這樣處理：小寫、去掉所有空白（中文辨識常在字中間加空白）
pub fn normalize(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).flat_map(char::to_lowercase).collect()
}

/// 存的鍵：Windows 不分大小寫，用小寫；其他平台照原樣（prune 檢查檔案還在不在時才找得到）
fn key(p: &str) -> String {
    if cfg!(windows) {
        p.to_lowercase()
    } else {
        p.to_string()
    }
}

fn stamp(path: &str) -> Option<(u64, f64)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.len(), mtime_ms(&m)))
}

impl OcrIndex {
    pub fn new(file: PathBuf) -> Self {
        let map = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Self {
            file,
            map: Mutex::new(map),
            running: AtomicBool::new(false),
            total: AtomicUsize::new(0),
            done: AtomicUsize::new(0),
            unavailable: AtomicBool::new(false),
            generation: std::sync::atomic::AtomicU64::new(1),
        }
    }

    fn save(&self) {
        let data = serde_json::to_vec(&*self.map.lock().unwrap_or_else(|e| e.into_inner())).unwrap_or_default();
        let _ = crate::paths::write_atomic(&self.file, &data);
    }

    /// 這張截圖辨識過、而且之後沒改過
    fn fresh(&self, path: &str) -> bool {
        let Some((bytes, mtime)) = stamp(path) else { return false };
        self.map.lock().unwrap_or_else(|e| e.into_inner()).get(&key(path)).is_some_and(|e| e.v >= VERSION && e.bytes == bytes && crate::paths::same_mtime(e.mtime, mtime))
    }

    /// 圖裡的字有沒有 query（已正規化；o 與 0、l 與 1 當成一樣）；還沒辨識的算沒有
    pub fn matches(&self, path: &str, query: &str) -> bool {
        if query.is_empty() {
            return false;
        }
        let q = fold(query);
        self.map.lock().unwrap_or_else(|e| e.into_inner()).get(&key(path)).is_some_and(|e| e.text.contains(query) || fold(&e.text).contains(&q))
    }

    /// 內容改過幾次
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    /// 全部辨識過的字：(路徑鍵, 小寫保留空白的字)；舊格式沒有 words 時用正規化的字
    pub fn docs(&self) -> Vec<(String, String)> {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(k, e)| (k.clone(), if e.words.is_empty() { e.text.clone() } else { e.words.clone() })).collect()
    }

    /// 查表用的鍵（和 docs() 的鍵一樣）
    pub fn key(path: &str) -> String {
        key(path)
    }

    /// 一張截圖的字（正規化過）；還沒辨識時 None
    pub fn text(&self, path: &str) -> Option<String> {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).get(&key(path)).map(|e| e.text.clone())
    }

    /// 進度：(做完, 總共)；沒有在做時 None
    pub fn progress(&self) -> Option<(usize, usize)> {
        self.running.load(Ordering::Relaxed).then(|| (self.done.load(Ordering::Relaxed), self.total.load(Ordering::Relaxed)))
    }

    pub fn available(&self) -> bool {
        !self.unavailable.load(Ordering::Relaxed)
    }

    /// 存一張的結果
    fn put(&self, path: &str, text: &str) {
        if let Some((bytes, mtime)) = stamp(path) {
            self.map.lock().unwrap_or_else(|e| e.into_inner()).insert(key(path), Entry { bytes, mtime, text: normalize(text), words: words(text), v: VERSION });
            self.generation.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 在背景辨識還沒辨識過的截圖（新的優先）；已經在做時不重複開始。recognize：讀圖並辨識，回傳字
    pub async fn build<F, Fut>(&self, mut paths: Vec<String>, recognize: F)
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<Output = Result<String, String>>,
    {
        if self.unavailable.load(Ordering::Relaxed) || self.running.swap(true, Ordering::SeqCst) {
            return;
        }
        paths.retain(|p| !self.fresh(p));
        self.total.store(paths.len(), Ordering::Relaxed);
        self.done.store(0, Ordering::Relaxed);
        let mut unsaved = 0;
        for p in paths {
            match recognize(p.clone()).await {
                Ok(text) => {
                    self.put(&p, &text);
                    unsaved += 1;
                }
                // 沒有可用的語言、不是 Windows：整個停下來
                Err(e) if crate::ocr::is_no_language(&e) || !cfg!(windows) => {
                    crate::info!("[文字索引] 無法辨識，停止：{e}");
                    self.unavailable.store(true, Ordering::Relaxed);
                    break;
                }
                // 這一張讀不到（壞檔、被刪掉）：存成空的，之後不再試（檔案改了才重做）
                Err(_) => self.put(&p, ""),
            }
            self.done.fetch_add(1, Ordering::Relaxed);
            if unsaved >= 20 {
                self.save();
                unsaved = 0;
            }
        }
        self.save();
        self.running.store(false, Ordering::SeqCst);
    }

    /// 刪掉已經不存在的檔案的紀錄。只刪「資料夾還在、檔案不見」的：資料夾讀不到（隨身碟、NAS 沒接上）時留著，
    /// 不然接回來後那裡的截圖全部要重新辨識
    pub fn prune(&self) {
        let keys: Vec<String> = self.map.lock().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect();
        // 檢查時不鎖住：截圖存在 NAS 上時可能要等，不要擋到搜尋
        let gone: std::collections::HashSet<String> = keys.into_iter().filter(|k| crate::paths::file_gone(Path::new(k))).collect();
        if gone.is_empty() {
            return;
        }
        // 檢查之後才又出現在同一個路徑的：留著
        self.map.lock().unwrap_or_else(|e| e.into_inner()).retain(|k, _| !gone.contains(k) || Path::new(k).exists());
        self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_ignores_case_and_spaces() {
        assert_eq!(normalize("502 Bad  Gateway"), "502badgateway");
        assert_eq!(normalize("登 入 畫 面"), "登入畫面");
    }

    #[tokio::test]
    async fn builds_once_and_matches() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png").display().to_string();
        let b = dir.path().join("b.png").display().to_string();
        std::fs::write(&a, "x").unwrap();
        std::fs::write(&b, "y").unwrap();
        let file = dir.path().join("ocr_index.json");
        let idx = OcrIndex::new(file.clone());
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let rec = |p: String| {
            calls.fetch_add(1, Ordering::SeqCst);
            async move { Ok::<_, String>(if p.ends_with("a.png") { "502 Bad Gateway".to_string() } else { "登 入 畫 面".to_string() }) }
        };
        idx.build(vec![a.clone(), b.clone()], rec).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(idx.matches(&a, &normalize("bad gate")));
        assert!(idx.matches(&b, &normalize("登入")));
        assert!(!idx.matches(&a, &normalize("登入")));
        // 重新開啟：讀檔，不用再辨識
        let idx2 = OcrIndex::new(file);
        idx2.build(vec![a.clone(), b.clone()], rec).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(idx2.matches(&a, "502"));
    }

    #[tokio::test]
    async fn prune_drops_deleted_but_keeps_unreachable() {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().join("here.png").display().to_string();
        let deleted = dir.path().join("deleted.png").display().to_string();
        let offline = dir.path().join("nas").join("offline.png").display().to_string();
        let file = dir.path().join("ocr_index.json");
        let idx = OcrIndex::new(file.clone());
        for p in [&here, &deleted] {
            std::fs::write(p, "x").unwrap();
        }
        std::fs::create_dir(dir.path().join("nas")).unwrap();
        std::fs::write(&offline, "x").unwrap();
        idx.build(vec![here.clone(), deleted.clone(), offline.clone()], |_| async { Ok::<_, String>("502 Bad Gateway".to_string()) }).await;
        // 截圖被刪掉；NAS 的資料夾讀不到（整個資料夾不見）
        std::fs::remove_file(&deleted).unwrap();
        std::fs::remove_dir_all(dir.path().join("nas")).unwrap();
        idx.prune();
        let again = OcrIndex::new(file);
        assert!(again.matches(&here, "502"));
        assert!(!again.matches(&deleted, "502"), "資料夾還在、檔案不見：刪掉");
        assert!(again.matches(&offline, "502"), "資料夾讀不到：留著");
    }

    #[tokio::test]
    async fn stops_when_ocr_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png").display().to_string();
        std::fs::write(&a, "x").unwrap();
        let idx = OcrIndex::new(dir.path().join("i.json"));
        idx.build(vec![a.clone(), a.clone()], |_| async { Err::<String, _>(crate::ocr::no_language().to_string()) }).await;
        assert!(!idx.available());
        assert_eq!(idx.progress(), None);
    }
}
