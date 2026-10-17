//! Jellyfin：錄完自動處理複製到媒體庫資料夾後，通知 Jellyfin 重新掃描，新影片馬上出現（不用等定時掃描）。
//! 自家伺服器多半在區域網路：直接連線，不經過 Proxy。

use crate::tr;
use crate::trf;
use serde_json::Value;
use std::time::Duration;

/// ui 設定裡的欄位名稱（是個人的連線資料，不放進設定組合）
pub const URL_KEY: &str = "jellyfinUrl";
pub const API_KEY: &str = "jellyfinKey";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jellyfin {
    /// 伺服器網址，例如 http://192.168.1.10:8096（結尾沒有 /）
    pub url: String,
    pub key: String,
}

impl Jellyfin {
    /// 網址和 API 金鑰都有填才算設定好
    pub fn from_ui(ui: Option<&Value>) -> Option<Jellyfin> {
        let o = ui?.as_object()?;
        let get = |k: &str| o.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
        Jellyfin::new(&get(URL_KEY), &get(API_KEY))
    }

    pub fn new(url: &str, key: &str) -> Option<Jellyfin> {
        let url = normalize_url(url)?;
        let key = key.trim();
        (!key.is_empty()).then(|| Jellyfin { url, key: key.to_string() })
    }

    /// 測試連線：成功回傳「伺服器名稱（版本）」
    pub fn test(&self) -> Result<String, String> {
        let r = self.call(agent().get(&format!("{}/System/Info", self.url)))?;
        let info: Value = r
            .into_string()
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .ok_or_else(|| tr!("回應看不懂，這個網址可能不是 Jellyfin", "Couldn't understand the reply. This address may not be Jellyfin").to_string())?;
        let name = info.get("ServerName").and_then(Value::as_str).unwrap_or("Jellyfin");
        Ok(match info.get("Version").and_then(Value::as_str) {
            Some(v) => format!("{name}（{v}）"),
            None => name.to_string(),
        })
    }

    /// 請 Jellyfin 重新掃描所有媒體庫（掃描在伺服器上跑，這裡不等它掃完）
    pub fn refresh(&self) -> Result<(), String> {
        self.call(agent().post(&format!("{}/Library/Refresh", self.url))).map(|_| ())
    }

    /// 請 Jellyfin 重新讀取這個檔名的影片（資訊與封面都換成檔案裡的）：改了封面、影片資訊時用。
    /// 只保留修改時間的改寫，媒體庫掃描不會發現，要逐支重新整理。回傳重新整理了幾支；
    /// Jellyfin 還沒有這支影片（0 支）時改成請它掃描媒體庫
    pub fn refresh_file(&self, file_name: &str) -> Result<usize, String> {
        let url = format!("{}/Items?recursive=true&includeItemTypes=Movie,Video,MusicVideo,Episode&fields=Path&enableImages=false&enableUserData=false", self.url);
        let body = self.call(agent().get(&url))?.into_string().map_err(|e| e.to_string())?;
        let ids = items_with_file(&body, file_name);
        if ids.is_empty() {
            self.refresh()?;
            return Ok(0);
        }
        for id in &ids {
            let url = format!("{}/Items/{id}/Refresh?metadataRefreshMode=FullRefresh&imageRefreshMode=FullRefresh&replaceAllMetadata=true&replaceAllImages=true", self.url);
            self.call(agent().post(&url))?;
        }
        Ok(ids.len())
    }

    /// 帶上 API 金鑰送出；失敗時回傳給使用者看的原因
    fn call(&self, req: ureq::Request) -> Result<ureq::Response, String> {
        req.set("Authorization", &format!("MediaBrowser Token=\"{}\"", self.key)).set("X-Emby-Token", &self.key).call().map_err(|e| describe(&e))
    }
}

/// /Items 的回應裡，檔名是 file_name 的項目編號（伺服器上的路徑和這台電腦看到的不一樣，只比對檔名，不分大小寫）
pub fn items_with_file(json: &str, file_name: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return vec![] };
    let want = file_name.to_lowercase();
    v.get("Items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|it| it.get("Path").and_then(Value::as_str).and_then(|p| p.rsplit(['/', '\\']).next()).is_some_and(|n| n.to_lowercase() == want))
                .filter_map(|it| it.get("Id").and_then(Value::as_str).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// 改了影片（封面、影片資訊）之後同步到 Jellyfin：
/// - 這支就在「複製」的資料夾裡（直接改媒體庫裡的檔案）：請 Jellyfin 重新讀取
/// - 改的是本機的原檔，而「複製」的資料夾裡有同名的一份：先用改好的檔案換掉那一份，再請 Jellyfin 重新讀取
///
/// 都不是（或沒設定）時回傳 None；有做事時回傳給使用者看的結果
pub fn sync_edited(jf: Option<&Jellyfin>, copy_to: &str, video: &std::path::Path) -> Option<Result<String, String>> {
    let name = video.file_name()?.to_string_lossy().into_owned();
    let copy_to = copy_to.trim();
    if copy_to.is_empty() {
        return None;
    }
    let lib = std::path::Path::new(copy_to);
    let in_library = video.parent().is_some_and(|d| same_dir(d, lib));
    let mut copied = false;
    if !in_library {
        let dest = lib.join(&name);
        if !dest.is_file() {
            return None;
        }
        if let Err(e) = std::fs::copy(video, &dest) {
            crate::warn!("[Jellyfin] 更新 {} 失敗：{e}", dest.display());
            return Some(Err(trf!("沒有更新「複製」資料夾裡的那一份：{e}", "Couldn't update the copy in the “Copy” folder: {e}")));
        }
        crate::info!("[Jellyfin] 已用改好的檔案換掉 {}", dest.display());
        copied = true;
    }
    let Some(jf) = jf else {
        return copied.then(|| Ok(tr!("也更新了「複製」資料夾裡的那一份", "The copy in the “Copy” folder was updated too").to_string()));
    };
    let done = if copied { tr!("已更新「複製」資料夾裡的那一份，", "Updated the copy in the “Copy” folder; ") } else { "" };
    Some(match jf.refresh_file(&name) {
        Ok(0) => Ok(format!("{done}{}", tr!("Jellyfin 還沒有這支影片，已請它掃描媒體庫", "Jellyfin doesn't have this video yet, so it was asked to scan"))),
        Ok(_) => Ok(format!("{done}{}", tr!("已請 Jellyfin 重新讀取這支影片", "Jellyfin was asked to reload this video"))),
        Err(e) => {
            crate::warn!("[Jellyfin] 重新整理 {name} 失敗：{e}");
            Err(format!("{done}{}", trf!("但沒有通知到 Jellyfin：{e}", "but Jellyfin couldn't be notified: {e}")))
        }
    })
}

/// 把檔案（連同旁邊的字幕）傳到「複製」的資料夾（NAS、Jellyfin 媒體庫）：同名的覆蓋（同一支影片的新版本），
/// 有設定 Jellyfin 時請它重新讀取（新的檔案就請它掃描）。剪輯、加速版另存好、或使用者按「傳到」時用。
/// 回傳給使用者看的結果；沒設定「複製」資料夾時 None
pub fn send_file(jf: Option<&Jellyfin>, copy_to: &str, file: &std::path::Path) -> Option<Result<String, String>> {
    let copy_to = copy_to.trim();
    if copy_to.is_empty() {
        return None;
    }
    let lib = std::path::Path::new(copy_to);
    let name = file.file_name()?.to_string_lossy().into_owned();
    let folder = lib.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| copy_to.to_string());
    // 就在那個資料夾裡：不用複製
    if !file.parent().is_some_and(|d| same_dir(d, lib)) {
        if !lib.is_absolute() {
            return Some(Err(tr!("「複製」的資料夾位置不對，請在設定裡重新選擇", "The “Copy” folder isn't valid. Choose it again in Settings").into()));
        }
        let files = crate::after_record::result_files(&file.display().to_string(), None);
        let copied = std::fs::create_dir_all(lib).and_then(|_| {
            for f in &files {
                if let Some(n) = f.file_name() {
                    std::fs::copy(f, lib.join(n))?;
                }
            }
            Ok(())
        });
        if let Err(e) = copied {
            crate::warn!("[傳到資料夾] {name} → {copy_to} 失敗：{e}");
            return Some(Err(trf!("沒有傳到「{folder}」：{e}", "Couldn't send it to “{folder}”: {e}")));
        }
        crate::info!("[傳到資料夾] {name}（{} 個檔案）→ {copy_to}", files.len());
    }
    let sent = trf!("已傳到「{folder}」：{name}", "Sent to “{folder}”: {name}");
    let Some(jf) = jf else { return Some(Ok(sent)) };
    Some(match jf.refresh_file(&name) {
        Ok(0) => Ok(format!("{sent}{}", tr!("，已請 Jellyfin 掃描", ", Jellyfin asked to scan"))),
        Ok(_) => Ok(format!("{sent}{}", tr!("，已請 Jellyfin 重新讀取", ", Jellyfin asked to reload it"))),
        Err(e) => {
            crate::warn!("[Jellyfin] 通知 {name} 失敗：{e}");
            Err(format!("{sent}{}", trf!("，但沒有通知到 Jellyfin：{e}", ", but Jellyfin couldn't be notified: {e}")))
        }
    })
}

/// 兩個資料夾是不是同一個（不分大小寫、/ 和 \ 都算、結尾的分隔符號不算）
fn same_dir(a: &std::path::Path, b: &std::path::Path) -> bool {
    let norm = |p: &std::path::Path| p.display().to_string().replace('\\', "/").trim_end_matches('/').to_lowercase();
    norm(a) == norm(b)
}

/// 使用者輸入的網址：補上 http://、去掉結尾的 /；空的或看不懂的回傳 None
pub fn normalize_url(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() || t.contains(char::is_whitespace) {
        return None;
    }
    let lower = t.to_ascii_lowercase();
    let (scheme, rest) = ["http://", "https://"].iter().find(|p| lower.starts_with(**p)).map_or(("http://", t), |p| (&t[..p.len()], &t[p.len()..]));
    let rest = rest.trim_end_matches('/');
    (!rest.is_empty() && !rest.starts_with('/')).then(|| format!("{scheme}{rest}"))
}

/// 區域網路的伺服器：直接連線（不經過 Proxy），等太久就放棄
fn agent() -> ureq::Agent {
    let b = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(8)).timeout_read(Duration::from_secs(15));
    #[cfg(windows)]
    let b = match native_tls::TlsConnector::new() {
        Ok(c) => b.tls_connector(std::sync::Arc::new(c)),
        Err(_) => b,
    };
    b.build()
}

fn describe(e: &ureq::Error) -> String {
    match e {
        ureq::Error::Status(401 | 403, _) => tr!("API 金鑰不對（Jellyfin 控制台 → API 金鑰）", "The API key is wrong (Jellyfin Dashboard → API Keys)").into(),
        ureq::Error::Status(404, _) => tr!("網址不對，找不到 Jellyfin（預設是 http://伺服器:8096）", "Wrong address; Jellyfin wasn't found there (usually http://server:8096)").into(),
        ureq::Error::Status(code, _) => trf!("Jellyfin 回應錯誤（HTTP {code}）", "Jellyfin returned an error (HTTP {code})"),
        ureq::Error::Transport(_) => {
            tr!("連不到 Jellyfin，請確認網址、伺服器有開著、和這台電腦在同一個網路", "Couldn't reach Jellyfin. Check the address, that the server is on, and that it's on the same network as this PC")
                .into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    #[test]
    fn urls_are_normalized() {
        assert_eq!(normalize_url(" 192.168.1.10:8096/ ").as_deref(), Some("http://192.168.1.10:8096"));
        assert_eq!(normalize_url("https://jf.example.com/jellyfin/").as_deref(), Some("https://jf.example.com/jellyfin"));
        assert_eq!(normalize_url("HTTP://nas:8096").as_deref(), Some("HTTP://nas:8096"));
        assert_eq!(normalize_url(""), None);
        assert_eq!(normalize_url("http://"), None);
        assert_eq!(normalize_url("my server"), None);
        assert!(Jellyfin::new("nas:8096", "  ").is_none());
        let ui = serde_json::json!({ URL_KEY: "nas:8096", API_KEY: " abc " });
        assert_eq!(Jellyfin::from_ui(Some(&ui)), Some(Jellyfin { url: "http://nas:8096".into(), key: "abc".into() }));
        assert!(Jellyfin::from_ui(Some(&serde_json::json!({ URL_KEY: "nas:8096" }))).is_none());
    }

    /// 假的 Jellyfin：回應一次，回傳收到的請求行與標頭
    fn serve_once(status: &str, body: &'static str) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let status = status.to_string();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut lines = vec![];
            let mut r = BufReader::new(s.try_clone().unwrap());
            loop {
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
                lines.push(line.trim().to_string());
            }
            write!(s, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            lines
        });
        (url, h)
    }

    /// 假的 Jellyfin：依序回應好幾個請求（每個連線一個），回傳收到的請求行
    fn serve_seq(replies: Vec<(&'static str, String)>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            let mut got = vec![];
            for (status, body) in replies {
                let (mut s, _) = l.accept().unwrap();
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut first = String::new();
                r.read_line(&mut first).unwrap();
                got.push(first.trim().to_string());
                loop {
                    let mut line = String::new();
                    r.read_line(&mut line).unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                }
                write!(s, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
            got
        });
        (url, h)
    }

    #[test]
    fn edited_video_is_reloaded_by_file_name() {
        let items = r#"{"Items":[{"Id":"a1","Path":"/media/videos/Rec_1.mp4"},{"Id":"b2","Path":"/media/videos/other.mp4"},{"Id":"c3","Path":"D:\\lib\\REC_1.MP4"}]}"#;
        assert_eq!(items_with_file(items, "rec_1.mp4"), ["a1", "c3"]);
        assert!(items_with_file("not json", "a.mp4").is_empty());
        let (url, h) = serve_seq(vec![("200 OK", items.to_string()), ("204 No Content", String::new()), ("204 No Content", String::new())]);
        assert_eq!(Jellyfin::new(&url, "k").unwrap().refresh_file("Rec_1.mp4").unwrap(), 2);
        let req = h.join().unwrap();
        assert!(req[0].starts_with("GET /Items?recursive=true&") && req[0].contains("fields=Path"), "{req:?}");
        assert!(req[1].starts_with("POST /Items/a1/Refresh?metadataRefreshMode=FullRefresh&imageRefreshMode=FullRefresh&replaceAllMetadata=true&replaceAllImages=true"), "{req:?}");
        assert!(req[2].starts_with("POST /Items/c3/Refresh?"), "{req:?}");
        // Jellyfin 還沒有這支：請它掃描媒體庫
        let (url, h) = serve_seq(vec![("200 OK", r#"{"Items":[]}"#.to_string()), ("204 No Content", String::new())]);
        assert_eq!(Jellyfin::new(&url, "k").unwrap().refresh_file("new.mp4").unwrap(), 0);
        assert!(h.join().unwrap()[1].starts_with("POST /Library/Refresh "));
    }

    #[test]
    fn edits_update_the_copy_in_the_library_folder() {
        let dir = tempfile::tempdir().unwrap();
        let (local, lib) = (dir.path().join("local"), dir.path().join("lib"));
        std::fs::create_dir_all(&local).unwrap();
        std::fs::create_dir_all(&lib).unwrap();
        let video = local.join("a.mp4");
        std::fs::write(&video, "new").unwrap();
        let copy_to = lib.display().to_string();
        // 媒體庫裡還沒有這一份、沒設定複製：不做事
        assert!(sync_edited(None, &copy_to, &video).is_none());
        assert!(sync_edited(None, "", &video).is_none());
        // 有同名的一份：換成改好的
        std::fs::write(lib.join("a.mp4"), "old").unwrap();
        assert!(sync_edited(None, &copy_to, &video).unwrap().is_ok());
        assert_eq!(std::fs::read_to_string(lib.join("a.mp4")).unwrap(), "new");
        // 直接改媒體庫裡的檔案（結尾有分隔符號、大小寫不同也算同一個資料夾）：不用複製，只通知
        let (url, h) = serve_seq(vec![("200 OK", r#"{"Items":[{"Id":"x","Path":"/m/a.mp4"}]}"#.to_string()), ("204 No Content", String::new())]);
        let jf = Jellyfin::new(&url, "k").unwrap();
        let msg = sync_edited(Some(&jf), &format!("{copy_to}/"), &lib.join("a.mp4")).unwrap().unwrap();
        assert!(!msg.contains("複製") && !msg.contains("Copy"), "{msg}");
        assert_eq!(h.join().unwrap().len(), 2);
    }

    #[test]
    fn send_copies_with_subtitles_and_replaces_older_versions() {
        let dir = tempfile::tempdir().unwrap();
        let (local, lib) = (dir.path().join("local"), dir.path().join("nas"));
        std::fs::create_dir_all(&local).unwrap();
        let video = local.join("Rec_1_cut.mp4");
        std::fs::write(&video, "v2").unwrap();
        std::fs::write(local.join("Rec_1_cut.srt"), "subs").unwrap();
        assert!(send_file(None, "", &video).is_none());
        // 資料夾還沒有：建立；字幕一起過去
        let copy_to = lib.display().to_string();
        let msg = send_file(None, &copy_to, &video).unwrap().unwrap();
        assert!(msg.contains("nas") && msg.contains("Rec_1_cut.mp4"), "{msg}");
        assert_eq!(std::fs::read_to_string(lib.join("Rec_1_cut.srt")).unwrap(), "subs");
        // 再傳一次新版本：覆蓋同名的，不會多出 _2
        std::fs::write(&video, "v3").unwrap();
        send_file(None, &copy_to, &video).unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(lib.join("Rec_1_cut.mp4")).unwrap(), "v3");
        assert_eq!(std::fs::read_dir(&lib).unwrap().count(), 2);
        // 有 Jellyfin：新檔案請它掃描
        let (url, h) = serve_seq(vec![("200 OK", r#"{"Items":[]}"#.to_string()), ("204 No Content", String::new())]);
        let jf = Jellyfin::new(&url, "k").unwrap();
        assert!(send_file(Some(&jf), &copy_to, &video).unwrap().is_ok());
        assert!(h.join().unwrap()[1].starts_with("POST /Library/Refresh "));
    }

    #[test]
    fn refresh_posts_with_the_api_key() {
        let (url, h) = serve_once("204 No Content", "");
        Jellyfin::new(&url, "k123").unwrap().refresh().unwrap();
        let req = h.join().unwrap();
        assert_eq!(req[0], "POST /Library/Refresh HTTP/1.1");
        assert!(req.iter().any(|l| l == "X-Emby-Token: k123"), "{req:?}");
        assert!(req.iter().any(|l| l == "Authorization: MediaBrowser Token=\"k123\""), "{req:?}");
    }

    #[test]
    fn test_reports_server_name_or_why_not() {
        let (url, h) = serve_once("200 OK", r#"{"ServerName":"家裡的NAS","Version":"10.10.7"}"#);
        assert_eq!(Jellyfin::new(&url, "k").unwrap().test().unwrap(), "家裡的NAS（10.10.7）");
        assert!(h.join().unwrap()[0].starts_with("GET /System/Info "));
        let (url, h) = serve_once("401 Unauthorized", "");
        assert!(Jellyfin::new(&url, "bad").unwrap().test().unwrap_err().contains("API"));
        h.join().unwrap();
        // 沒有開的連接埠
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert!(Jellyfin::new(&format!("127.0.0.1:{port}"), "k").unwrap().refresh().is_err());
    }
}
