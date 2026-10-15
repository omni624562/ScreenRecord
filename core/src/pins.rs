//! 釘選（我的最愛）：重要的截圖、錄影加星號，「選取全部」「全選本頁」和「清理舊截圖」都會略過。
//! 存在資料夾裡的 pins.json（完整路徑的清單）；檔案改名、搬移時跟著改，刪除時拿掉。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

pub struct PinStore {
    file: PathBuf,
    /// 小寫路徑 → 原本的路徑
    set: Mutex<BTreeMap<String, String>>,
}

impl PinStore {
    pub fn new(file: PathBuf) -> Self {
        let list: Vec<String> = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let set = list.into_iter().map(|p| (p.to_lowercase(), p)).collect();
        Self { file, set: Mutex::new(set) }
    }

    fn save(&self, set: &BTreeMap<String, String>) {
        if let Some(dir) = self.file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let list: Vec<&String> = set.values().collect();
        let tmp = self.file.with_extension("json.tmp");
        if std::fs::write(&tmp, serde_json::to_vec(&list).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(&tmp, &self.file);
        }
    }

    pub fn contains(&self, path: &str) -> bool {
        self.set.lock().unwrap().contains_key(&path.to_lowercase())
    }

    pub fn set(&self, path: &str, on: bool) {
        let mut set = self.set.lock().unwrap();
        let changed = if on { set.insert(path.to_lowercase(), path.to_string()).is_none() } else { set.remove(&path.to_lowercase()).is_some() };
        if changed {
            self.save(&set);
        }
    }

    /// 全部釘選的檔案（還存在的）
    pub fn list(&self) -> Vec<String> {
        self.set.lock().unwrap().values().filter(|p| std::path::Path::new(p).is_file()).cloned().collect()
    }

    /// 檔案改名或搬到別的資料夾
    pub fn renamed(&self, from: &str, to: &str) {
        let mut set = self.set.lock().unwrap();
        if set.remove(&from.to_lowercase()).is_some() {
            set.insert(to.to_lowercase(), to.to_string());
            self.save(&set);
        }
    }

    pub fn removed(&self, path: &str) {
        self.set(path, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_rename_remove_persist() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("A.png").display().to_string();
        let b = dir.path().join("B.png").display().to_string();
        std::fs::write(&a, "x").unwrap();
        let file = dir.path().join("pins.json");
        let pins = PinStore::new(file.clone());
        pins.set(&a, true);
        assert!(pins.contains(&a.to_uppercase()), "不分大小寫");
        assert_eq!(pins.list(), vec![a.clone()]);
        // 重新開啟還在
        assert!(PinStore::new(file.clone()).contains(&a));
        std::fs::rename(&a, &b).unwrap();
        pins.renamed(&a, &b);
        assert!(!pins.contains(&a) && pins.contains(&b));
        pins.removed(&b);
        assert!(!PinStore::new(file).contains(&b));
    }
}
