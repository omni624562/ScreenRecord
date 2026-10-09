//! 剪輯專案：每個剪輯版（*_cut.mp4）記住用哪支原始影片、怎麼剪、加了哪些標註，
//! 之後可以從原始影片重新套用並修改，再取代那個剪輯版（標註燒進影片後就改不了，所以要另外記）。
//! 存在資料夾 edits/ 底下，檔名是剪輯版完整路徑（小寫）的雜湊；內容是介面給的 JSON，這裡不解讀。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// 專案資料的上限（標註本身很小；圖片在匯出時才由介面重新畫）
pub const MAX_PROJECT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    /// 剪輯版（輸出檔）的完整路徑
    pub output: String,
    /// 原始影片的完整路徑
    pub source: String,
    pub saved_at: u64,
    /// 介面的剪輯設定與標註（原樣保存）
    pub data: Value,
}

pub struct ProjectStore {
    dir: PathBuf,
}

fn key(path: &str) -> String {
    let h = Sha256::digest(path.to_lowercase().as_bytes());
    h.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

fn same(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

impl ProjectStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn file(&self, output: &str) -> PathBuf {
        self.dir.join(format!("{}.json", key(output)))
    }

    fn read(path: &Path) -> Option<Project> {
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    fn write(&self, p: &Project) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let file = self.file(&p.output);
        let tmp = file.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec(p).unwrap_or_default())?;
        std::fs::rename(&tmp, &file)
    }

    fn all(&self) -> Vec<(PathBuf, Project)> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return vec![] };
        rd.flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .filter_map(|p| Self::read(&p).map(|x| (p, x)))
            .collect()
    }

    pub fn save(&self, output: &str, source: &str, data: Value) -> std::io::Result<()> {
        self.write(&Project { output: output.into(), source: source.into(), saved_at: crate::paths::now_ms(), data })
    }

    /// 剪輯版的專案（剪輯版已不存在就不算）
    pub fn for_output(&self, output: &str) -> Option<Project> {
        let p = Self::read(&self.file(output))?;
        (same(&p.output, output) && Path::new(output).is_file()).then_some(p)
    }

    /// 用這支原始影片做的剪輯中，最近儲存、而且剪輯版還在的一個
    pub fn latest_for_source(&self, source: &str) -> Option<Project> {
        self.all()
            .into_iter()
            .map(|(_, p)| p)
            .filter(|p| same(&p.source, source) && Path::new(&p.output).is_file())
            .max_by_key(|p| p.saved_at)
    }

    /// 檔案改名：剪輯版改名就搬專案，原始影片改名就更新指向它的專案
    pub fn renamed(&self, from: &str, to: &str) {
        for (file, mut p) in self.all() {
            let mut changed = false;
            if same(&p.source, from) {
                p.source = to.into();
                changed = true;
            }
            if same(&p.output, from) {
                p.output = to.into();
                let _ = std::fs::remove_file(&file);
                changed = true;
            }
            if changed {
                let _ = self.write(&p);
            }
        }
    }

    /// 剪輯版被刪除：專案也不需要了（原始影片被刪除時保留，剪輯版仍可在找不到原片時提示）
    pub fn removed(&self, path: &str) {
        let _ = std::fs::remove_file(self.file(path));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn save_find_rename_remove() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProjectStore::new(dir.path().join("edits"));
        let src = dir.path().join("Rec.mp4").display().to_string();
        let out = dir.path().join("Rec_cut.mp4").display().to_string();
        std::fs::write(&src, "x").unwrap();
        // 剪輯版還不存在：找不到
        store.save(&out, &src, json!({ "anns": [1] })).unwrap();
        assert!(store.for_output(&out).is_none());
        std::fs::write(&out, "x").unwrap();
        let p = store.for_output(&out).unwrap();
        assert_eq!(p.data, json!({ "anns": [1] }));
        assert_eq!(store.latest_for_source(&src).unwrap().output, out);

        // 原片改名：專案跟著指向新名稱
        let src2 = dir.path().join("Demo.mp4").display().to_string();
        std::fs::rename(&src, &src2).unwrap();
        store.renamed(&src, &src2);
        assert_eq!(store.for_output(&out).unwrap().source, src2);

        // 剪輯版改名：專案搬到新名稱
        let out2 = dir.path().join("Demo_cut.mp4").display().to_string();
        std::fs::rename(&out, &out2).unwrap();
        store.renamed(&out, &out2);
        assert!(store.for_output(&out).is_none());
        assert_eq!(store.for_output(&out2).unwrap().source, src2);
        assert_eq!(store.latest_for_source(&src2).unwrap().output, out2);

        store.removed(&out2);
        assert!(store.for_output(&out2).is_none());
        assert!(store.latest_for_source(&src2).is_none());
    }
}
