//! 截圖的來歷：截圖當下在用的程式與視窗標題（例如 chrome.exe「訂單查詢 - Google Chrome」），
//! 以及怎麼截的（長截圖、影片畫面、拼圖、編輯過）。「全部截圖」的瀏覽欄用它分類。
//!
//! 存在 shot_meta.json（路徑 → 來歷）；重新命名、搬移時跟著改，刪除時移除。只有加上這功能之後的截圖才有。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ShotMeta {
    /// 執行檔名稱（小寫，例如 chrome.exe）；不知道時空白
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub app: String,
    /// 視窗標題
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// 怎麼截的：long（長截圖）、frame（影片畫面）、combine（拼成一張）、edit（編輯後另存）；一般截圖空白
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub src: String,
}

pub struct ShotMetaStore {
    file: PathBuf,
    map: Mutex<HashMap<String, ShotMeta>>,
}

fn key(p: &str) -> String {
    if cfg!(windows) {
        p.to_lowercase()
    } else {
        p.to_string()
    }
}

/// 執行檔名稱 → 好認的名稱（chrome.exe → Chrome）；不認得的去掉 .exe、第一個字大寫
pub fn app_label(exe: &str) -> String {
    let stem = exe.trim_end_matches(".exe");
    let known = [
        ("chrome", "Chrome"),
        ("msedge", "Edge"),
        ("firefox", "Firefox"),
        ("brave", "Brave"),
        ("opera", "Opera"),
        ("line", "LINE"),
        ("ms-teams", "Teams"),
        ("teams", "Teams"),
        ("slack", "Slack"),
        ("discord", "Discord"),
        ("telegram", "Telegram"),
        ("whatsapp", "WhatsApp"),
        ("excel", "Excel"),
        ("winword", "Word"),
        ("powerpnt", "PowerPoint"),
        ("outlook", "Outlook"),
        ("olk", "Outlook"),
        ("onenote", "OneNote"),
        ("acrord32", "Acrobat"),
        ("acrobat", "Acrobat"),
        ("code", "VS Code"),
        ("devenv", "Visual Studio"),
        ("explorer", "檔案總管"),
        ("notepad", "記事本"),
        ("mspaint", "小畫家"),
        ("windowsterminal", "終端機"),
        ("cmd", "命令提示字元"),
        ("powershell", "PowerShell"),
        ("pwsh", "PowerShell"),
        ("zoom", "Zoom"),
        ("figma", "Figma"),
        ("notion", "Notion"),
        ("obsidian", "Obsidian"),
    ];
    if let Some((_, l)) = known.iter().find(|(k, _)| *k == stem) {
        if crate::i18n::is_en() {
            return match *l {
                "檔案總管" => "File Explorer",
                "記事本" => "Notepad",
                "小畫家" => "Paint",
                "終端機" => "Terminal",
                "命令提示字元" => "Command Prompt",
                x => x,
            }
            .to_string();
        }
        return l.to_string();
    }
    let mut c = stem.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

impl ShotMetaStore {
    pub fn new(file: PathBuf) -> Self {
        let map = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Self { file, map: Mutex::new(map) }
    }

    fn save(&self, map: &HashMap<String, ShotMeta>) {
        let _ = crate::paths::write_atomic(&self.file, &serde_json::to_vec(map).unwrap_or_default());
    }

    /// 記下一張截圖的來歷（全部空白時不記）
    pub fn record(&self, path: &str, meta: ShotMeta) {
        if meta == ShotMeta::default() {
            return;
        }
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        map.insert(key(path), meta);
        self.save(&map);
    }

    pub fn get(&self, path: &str) -> Option<ShotMeta> {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).get(&key(path)).cloned()
    }

    /// 全部（路徑鍵 → 來歷），分類時一次取用
    pub fn all(&self) -> HashMap<String, ShotMeta> {
        self.map.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 查表用的鍵（和 all() 的鍵一樣）
    pub fn key(path: &str) -> String {
        key(path)
    }

    /// 編輯後另存：沿用原圖的程式與標題，標成編輯過
    pub fn copy_edited(&self, from: &str, to: &str) {
        let mut m = self.get(from).unwrap_or_default();
        m.src = "edit".into();
        self.record(to, m);
    }

    pub fn renamed(&self, from: &str, to: &str) {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(m) = map.remove(&key(from)) {
            map.insert(key(to), m);
            self.save(&map);
        }
    }

    pub fn removed(&self, path: &str) {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if map.remove(&key(path)).is_some() {
            self.save(&map);
        }
    }

    /// 刪掉在程式外被刪掉的截圖的紀錄（不然這個檔案只會越來越大，每次截圖都要整份重寫）。
    /// 只刪「資料夾還在、檔案不見」的：資料夾讀不到（隨身碟、NAS 沒接上）時留著，接上後還用得到
    pub fn prune(&self) {
        let keys: Vec<String> = self.map.lock().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect();
        // 檢查時不鎖住：截圖存在 NAS 上時可能要等，不要擋到正在記錄的截圖
        let gone: std::collections::HashSet<String> = keys.into_iter().filter(|k| is_gone(Path::new(k))).collect();
        if gone.is_empty() {
            return;
        }
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        // 檢查之後才又截到同一個路徑的：留著
        map.retain(|k, _| !gone.contains(k) || Path::new(k).exists());
        self.save(&map);
        crate::info!("[截圖來歷] 清掉 {} 筆已經不在的截圖", gone.len());
    }
}

/// 檔案不見了，而且確定不是讀不到：所在的資料夾還在
fn is_gone(p: &Path) -> bool {
    !p.exists() && p.parent().is_some_and(Path::is_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_renames_and_removes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("shot_meta.json");
        let s = ShotMetaStore::new(file.clone());
        s.record("C:/a.png", ShotMeta { app: "chrome.exe".into(), title: "訂單".into(), src: String::new() });
        s.record("C:/empty.png", ShotMeta::default());
        assert!(s.get("C:/empty.png").is_none(), "空白的不記");
        s.copy_edited("C:/a.png", "C:/a_編輯.png");
        assert_eq!(s.get("C:/a_編輯.png").unwrap().src, "edit");
        s.renamed("C:/a.png", "C:/b.png");
        let again = ShotMetaStore::new(file);
        assert_eq!(again.get("C:/b.png").unwrap().app, "chrome.exe");
        again.removed("C:/b.png");
        assert!(again.get("C:/b.png").is_none());
    }

    #[test]
    fn prune_drops_deleted_but_keeps_unreachable() {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().join("here.png");
        std::fs::write(&here, b"").unwrap();
        let deleted = dir.path().join("deleted.png");
        let offline = dir.path().join("nas").join("offline.png");
        let file = dir.path().join("shot_meta.json");
        let s = ShotMetaStore::new(file.clone());
        let m = ShotMeta { app: "chrome.exe".into(), ..Default::default() };
        for p in [&here, &deleted, &offline] {
            s.record(&p.display().to_string(), m.clone());
        }
        s.prune();
        let again = ShotMetaStore::new(file);
        assert!(again.get(&here.display().to_string()).is_some());
        assert!(again.get(&deleted.display().to_string()).is_none(), "資料夾還在、檔案不見：刪掉");
        assert!(again.get(&offline.display().to_string()).is_some(), "資料夾讀不到：留著");
    }

    #[test]
    fn friendly_app_names() {
        assert_eq!(app_label("chrome.exe"), "Chrome");
        assert_eq!(app_label("line.exe"), "LINE");
        assert_eq!(app_label("myapp.exe"), "Myapp");
    }
}
