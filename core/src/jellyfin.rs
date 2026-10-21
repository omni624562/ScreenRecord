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
    /// 這台電腦上的媒體庫資料夾（例如 Y:\）：知道它在伺服器上叫什麼時，傳好只掃描那個資料夾
    pub root: String,
    /// 傳好之後依資料夾放進 Jellyfin 的合集（family/2024 → 「family 2024」）
    pub collections: bool,
}

/// ui 設定裡「依資料夾放進合集」的欄位名稱（沒設定時是要）
pub const COLLECTIONS_KEY: &str = "jellyfinCollections";

/// 測試連線時看媒體庫設定的結果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryCheck {
    /// 給使用者看的說明
    pub note: String,
    /// 類型、選項都好了
    pub ok: bool,
}

/// 這台電腦的媒體庫資料夾在伺服器上的路徑（網址|資料夾 → 路徑、什麼時候查的）；查不到的 5 分鐘內不再查
static SERVER_ROOTS: std::sync::Mutex<Option<std::collections::HashMap<String, RootLookup>>> = std::sync::Mutex::new(None);
type RootLookup = (Option<String>, std::time::Instant);

impl Jellyfin {
    /// 網址和 API 金鑰都有填才算設定好；媒體庫資料夾也一起帶著（只掃描放進去的資料夾用）
    pub fn from_ui(ui: Option<&Value>) -> Option<Jellyfin> {
        let o = ui?.as_object()?;
        let get = |k: &str| o.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
        let collections = o.get(COLLECTIONS_KEY).and_then(Value::as_bool).unwrap_or(true);
        Jellyfin::new(&get(URL_KEY), &get(API_KEY)).map(|j| Jellyfin { collections, ..j.with_root(&get(crate::after_record::COPY_KEY)) })
    }

    pub fn new(url: &str, key: &str) -> Option<Jellyfin> {
        let url = normalize_url(url)?;
        let key = key.trim();
        (!key.is_empty()).then(|| Jellyfin { url, key: key.to_string(), root: String::new(), collections: true })
    }

    /// 帶著這台電腦上的媒體庫資料夾
    pub fn with_root(mut self, root: &str) -> Jellyfin {
        self.root = root.trim().to_string();
        self
    }

    /// 媒體庫資料夾在伺服器上的路徑（例如 Y:\ → /volume1/video）：從 Jellyfin 最近加入的影片推出來（伺服器上的路徑後面幾層，
    /// 在這台電腦的媒體庫資料夾底下也找得到同一個檔案）。媒體庫還沒有影片、或對不上時 None
    pub fn server_root(&self) -> Option<String> {
        if self.root.is_empty() {
            return None;
        }
        let key = format!("{}|{}", self.url, self.root.to_lowercase());
        if let Some((found, at)) = SERVER_ROOTS.lock().unwrap().as_ref().and_then(|m| m.get(&key).cloned()) {
            if found.is_some() || at.elapsed() < Duration::from_secs(300) {
                return found;
            }
        }
        let url = format!(
            "{}/Items?recursive=true&includeItemTypes=Movie,Video,MusicVideo,Episode&fields=Path&enableImages=false&enableUserData=false&sortBy=DateCreated&sortOrder=Descending&limit=200",
            self.url
        );
        let found = self.call(agent().get(&url)).ok().and_then(|r| r.into_string().ok()).and_then(|body| server_root_from(&body, &self.root));
        if let Some(r) = &found {
            crate::info!("[Jellyfin] 媒體庫 {} 在伺服器上是 {r}", self.root);
        }
        SERVER_ROOTS.lock().unwrap().get_or_insert_with(Default::default).insert(key, (found.clone(), std::time::Instant::now()));
        found
    }

    /// 請 Jellyfin 掃描剛放進去的檔案：知道伺服器上的路徑時只掃描它所在的資料夾（幾秒就出現），
    /// 不知道時掃描所有媒體庫。回傳是不是只掃描了那個資料夾
    pub fn scan(&self, local: &std::path::Path) -> Result<bool, String> {
        let dir = local.parent().unwrap_or(local);
        let Some(remote) = self.server_root().and_then(|r| server_path(&r, &self.root, dir)) else {
            return self.refresh().map(|_| false);
        };
        let body = serde_json::json!({ "Updates": [{ "Path": remote, "UpdateType": "Created" }] }).to_string();
        match self.call_body(agent().post(&format!("{}/Library/Media/Updated", self.url)), &body) {
            Ok(_) => {
                crate::info!("[Jellyfin] 請它掃描 {remote}");
                Ok(true)
            }
            // 舊版或不支援：掃描所有媒體庫
            Err(_) => self.refresh().map(|_| false),
        }
    }

    /// 媒體庫的設定看起來對不對：媒體庫資料夾在伺服器上對應到哪個 Jellyfin 媒體庫、類型是不是「家庭影片和照片」。
    /// 回傳給使用者看的說明（測試連線時接在後面）；看不出來時 None
    pub fn check_library(&self) -> Option<LibraryCheck> {
        if self.root.is_empty() {
            return None;
        }
        let root = self.server_root();
        let libs = self.call(agent().get(&format!("{}/Library/VirtualFolders", self.url))).ok()?.into_string().ok()?;
        library_check(&libs, root.as_deref())
    }

    /// 一鍵修好媒體庫設定：媒體庫資料夾所在的「家庭影片和照片」媒體庫打開即時監控、擷取章節圖片、時間軸預覽縮圖；
    /// 沒有這種媒體庫時新建一個指到同一個資料夾。回傳做了什麼
    pub fn fix_library(&self) -> Result<String, String> {
        let root = self.server_root().ok_or_else(|| {
            tr!(
                "Jellyfin 的媒體庫裡還沒有從這個資料夾來的影片，看不出資料夾在伺服器上的位置：先傳一支過去再按",
                "Jellyfin has no videos from this folder yet, so its location on the server is unknown. Send one video first"
            )
            .to_string()
        })?;
        let body = self.call(agent().get(&format!("{}/Library/VirtualFolders", self.url)))?.into_string().map_err(|e| e.to_string())?;
        let libs: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        let home: Vec<&Value> = libs.as_array().map(|a| a.iter().filter(|l| covers(l, &root) && l.get("CollectionType").and_then(Value::as_str) == Some("homevideos")).collect()).unwrap_or_default();
        if home.is_empty() {
            // 新建：名稱撞到時加上 (2)、(3)…
            let names: Vec<&str> = libs.as_array().map(|a| a.iter().filter_map(|l| l.get("Name").and_then(Value::as_str)).collect()).unwrap_or_default();
            let base = tr!("錄影", "Recordings");
            let name = (1..100).map(|i| if i == 1 { base.to_string() } else { format!("{base} ({i})") }).find(|n| !names.contains(&n.as_str())).unwrap_or_else(|| base.to_string());
            let opts = serde_json::json!({ "LibraryOptions": with_good_options(&serde_json::json!({ "PathInfos": [{ "Path": root }] })) });
            let req = agent().post(&format!("{}/Library/VirtualFolders", self.url)).query("name", &name).query("collectionType", "homevideos").query("paths", &root).query("refreshLibrary", "true");
            self.call_body(req, &opts.to_string())?;
            crate::info!("[Jellyfin] 新建家庭影片媒體庫「{name}」：{root}");
            let old: Vec<&str> = libs.as_array().map(|a| a.iter().filter(|l| covers(l, &root)).filter_map(|l| l.get("Name").and_then(Value::as_str)).collect()).unwrap_or_default();
            let mut msg = trf!("已新建「家庭影片和照片」媒體庫「{name}」，指到 {root}", "Created the Home Videos and Photos library “{name}” for {root}");
            if !old.is_empty() {
                msg.push_str(&trf!(
                    "。原本的「{}」也會列出同一批影片，可以到 Jellyfin 控制台把這個資料夾從它移除",
                    ". “{}” still lists the same videos; you can remove this folder from it in the Jellyfin Dashboard",
                    old.join("、")
                ));
            }
            return Ok(msg);
        }
        for l in &home {
            let id = l.get("ItemId").and_then(Value::as_str).ok_or_else(|| tr!("Jellyfin 的媒體庫資料看不懂", "Couldn't understand Jellyfin's library data").to_string())?;
            let opts = with_good_options(l.get("LibraryOptions").unwrap_or(&Value::Null));
            self.call_body(agent().post(&format!("{}/Library/VirtualFolders/LibraryOptions", self.url)), &serde_json::json!({ "Id": id, "LibraryOptions": opts }).to_string())?;
        }
        let names: Vec<&str> = home.iter().filter_map(|l| l.get("Name").and_then(Value::as_str)).collect();
        crate::info!("[Jellyfin] 打開媒體庫選項：{}", names.join("、"));
        Ok(trf!("已打開「{}」的即時監控、擷取章節圖片、時間軸預覽縮圖", "Turned on real-time monitoring, chapter images and trickplay for “{}”", names.join("、")))
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

    /// 伺服器的編號（做連結時用；拿不到時連結不帶）
    pub fn server_id(&self) -> Option<String> {
        let r = self.call(agent().get(&format!("{}/System/Info", self.url))).ok()?;
        let info: Value = serde_json::from_str(&r.into_string().ok()?).ok()?;
        info.get("Id").and_then(Value::as_str).map(String::from)
    }

    /// Jellyfin 裡檔名是 file_name 的影片編號（沒有時空的）
    pub fn find(&self, file_name: &str) -> Result<Vec<String>, String> {
        let url = format!("{}/Items?recursive=true&includeItemTypes=Movie,Video,MusicVideo,Episode&fields=Path&enableImages=false&enableUserData=false", self.url);
        let body = self.call(agent().get(&url))?.into_string().map_err(|e| e.to_string())?;
        Ok(items_with_file(&body, file_name))
    }

    /// 請 Jellyfin 掃描後等它掃到這支影片（每 3 秒問一次，最多等 wait）：回傳影片編號；時間到還沒有時 None
    pub fn wait_for(&self, file_name: &str, wait: Duration) -> Result<Option<String>, String> {
        let start = std::time::Instant::now();
        loop {
            if let Some(id) = self.find(file_name)?.into_iter().next() {
                return Ok(Some(id));
            }
            if start.elapsed() >= wait {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_secs(3).min(wait));
        }
    }

    /// 在瀏覽器打開這支影片的網址（可以貼給別人；對方要能連到這台 Jellyfin、有帳號）
    pub fn link(&self, id: &str, server_id: Option<&str>) -> String {
        match server_id {
            Some(sid) => format!("{}/web/#/details?id={id}&serverId={sid}", self.url),
            None => format!("{}/web/#/details?id={id}", self.url),
        }
    }

    /// 把影片資訊（標題、說明、類型、年份、標籤；空的不動 Jellyfin 上的）設到這支影片：取回整筆資料、換上去再存回去。
    /// Jellyfin 預設不用影片檔裡的標題、也不一定讀說明，所以直接寫過去
    pub fn set_details(&self, id: &str, x: &Extras) -> Result<(), String> {
        if !x.has_details() {
            return Ok(());
        }
        let body = match self.call(agent().get(&format!("{}/Items/{id}", self.url))) {
            Ok(r) => r.into_string().map_err(|e| e.to_string())?,
            // 舊版 Jellyfin 要指定使用者：用第一個管理員
            Err(_) => {
                let uid = self.admin_id()?;
                self.call(agent().get(&format!("{}/Users/{uid}/Items/{id}", self.url)))?.into_string().map_err(|e| e.to_string())?
            }
        };
        let item = with_details(&body, x).ok_or_else(|| tr!("Jellyfin 的影片資料看不懂", "Couldn't understand Jellyfin's item data").to_string())?;
        self.call_body(agent().post(&format!("{}/Items/{id}", self.url)), &item).map(|_| ())
    }

    /// 換掉這支影片在 Jellyfin 的封面（主要圖片）：圖片用 base64 送過去
    /// 換封面時橫幅圖、縮圖也一起換（不然首頁還是舊的那一格）
    pub fn set_cover(&self, id: &str, jpeg: &[u8]) -> Result<(), String> {
        self.set_image(id, "Primary", jpeg)?;
        for kind in ["Backdrop", "Thumb"] {
            if let Err(e) = self.set_image(id, kind, jpeg) {
                crate::info!("[Jellyfin] 換 {kind} 圖失敗：{e}");
            }
        }
        Ok(())
    }

    fn admin_id(&self) -> Result<String, String> {
        let body = self.call(agent().get(&format!("{}/Users", self.url)))?.into_string().map_err(|e| e.to_string())?;
        let users: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        users
            .as_array()
            .and_then(|u| u.iter().find(|x| x.pointer("/Policy/IsAdministrator").and_then(Value::as_bool) == Some(true)).or(u.first()))
            .and_then(|x| x.get("Id").and_then(Value::as_str))
            .map(String::from)
            .ok_or_else(|| tr!("Jellyfin 沒有使用者", "Jellyfin has no users").to_string())
    }

    /// 瀏覽用的使用者：有選「星號同步到」的帳號就用它（看得到它的觀看紀錄），不然用第一個管理員
    pub fn browse_user(&self, preferred: &str) -> Result<String, String> {
        if preferred.is_empty() {
            self.admin_id()
        } else {
            Ok(preferred.to_string())
        }
    }

    /// 這個使用者看得到的媒體庫（最上層）
    pub fn views(&self, user: &str) -> Result<Vec<JfItem>, String> {
        let body = self.call(agent().get(&format!("{}/Users/{user}/Views", self.url)))?.into_string().map_err(|e| e.to_string())?;
        Ok(parse_items(&body).0.into_iter().map(|i| JfItem { kind: JfKind::Folder, ..i }).collect())
    }

    /// 一個資料夾（媒體庫、合集）裡的東西：資料夾在前，影片依 sort 排；search 不是空的時找整個伺服器。
    /// filter 不是「全部」時列出這個資料夾底下（含子資料夾）符合的影片；facet（人物、標籤、年份）找整個伺服器的影片。
    /// 回傳（這一頁, 全部幾個）
    #[allow(clippy::too_many_arguments)]
    pub fn items(&self, user: &str, parent: Option<&str>, search: &str, start: usize, limit: usize, sort: JfSort, filter: JfFilter, facet: Option<&JfFacet>) -> Result<(Vec<JfItem>, usize), String> {
        let mut req = self.items_req(&format!("{}/Users/{user}/Items", self.url)).query("startIndex", &start.to_string()).query("limit", &limit.to_string()).query("enableTotalRecordCount", "true");
        let (by, order) = sort.query();
        let search = search.trim();
        if let Some(f) = facet {
            req = req.query("sortBy", by).query("sortOrder", order).query("recursive", "true").query("includeItemTypes", VIDEO_TYPES);
            req = match f {
                JfFacet::Person(id, _) => req.query("personIds", id),
                JfFacet::Tag(t) => req.query("tags", t),
                JfFacet::Year(y) => req.query("years", &y.to_string()),
            };
        } else if !search.is_empty() {
            req = req.query("searchTerm", search).query("recursive", "true").query("includeItemTypes", "Video,Movie,Episode,MusicVideo,Folder,BoxSet");
        } else if filter == JfFilter::All {
            req = req.query("sortBy", &format!("IsFolder,{by}")).query("sortOrder", &format!("Descending,{order}"));
        } else {
            req = req.query("sortBy", by).query("sortOrder", order).query("recursive", "true").query("includeItemTypes", VIDEO_TYPES);
        }
        if search.is_empty() && facet.is_none() {
            if let Some(p) = parent {
                req = req.query("parentId", p);
            }
        }
        if let Some(f) = filter.query() {
            req = req.query("filters", f);
        }
        let body = self.call(req)?.into_string().map_err(|e| e.to_string())?;
        Ok(parse_items(&body))
    }

    /// 清單要的欄位、縮圖
    fn items_req(&self, url: &str) -> ureq::Request {
        agent().get(url).query("fields", "Path,MediaStreams,DateCreated,PremiereDate,ChildCount,Chapters,Trickplay").query("enableImageTypes", "Primary").query("imageTypeLimit", "1")
    }

    /// 看到一半的影片（Jellyfin 首頁的「繼續觀看」，最近看的在前）
    pub fn resume_items(&self, user: &str, limit: usize) -> Result<Vec<JfItem>, String> {
        let req = self.items_req(&format!("{}/Users/{user}/Items/Resume", self.url)).query("limit", &limit.to_string()).query("mediaTypes", "Video");
        let body = self.call(req)?.into_string().map_err(|e| e.to_string())?;
        Ok(parse_items(&body).0)
    }

    /// 最近加入的影片（Jellyfin 首頁的「最近加入」，新的在前）
    pub fn latest_items(&self, user: &str, limit: usize) -> Result<Vec<JfItem>, String> {
        let req = self.items_req(&format!("{}/Users/{user}/Items/Latest", self.url)).query("limit", &limit.to_string()).query("includeItemTypes", VIDEO_TYPES).query("groupItems", "false");
        let body = self.call(req)?.into_string().map_err(|e| e.to_string())?;
        Ok(parse_items(&body).0.into_iter().filter(|i| i.kind == JfKind::Video).collect())
    }

    /// 記下 user 看到哪裡（秒；看完了 played = true，下次從頭開始）：Jellyfin 首頁的「繼續觀看」、電視和手機都接得上。
    /// 10.10 以後是 /UserItems/{id}/UserData，10.9 是 /Users/{user}/Items/{id}/UserData，更舊的版本用「停止播放」
    pub fn save_position(&self, user: &str, id: &str, at: f64, played: bool) -> Result<(), String> {
        let ticks = if played { 0 } else { (at.max(0.0) * 1e7) as u64 };
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S.000Z").to_string();
        let mut body = serde_json::json!({ "PlaybackPositionTicks": ticks, "LastPlayedDate": now });
        if played {
            body["Played"] = Value::Bool(true);
        }
        let body = body.to_string();
        let new = self.call_body(agent().post(&format!("{}/UserItems/{id}/UserData", self.url)).query("userId", user), &body);
        if new.is_ok() {
            return Ok(());
        }
        if self.call_body(agent().post(&format!("{}/Users/{user}/Items/{id}/UserData", self.url)), &body).is_ok() {
            return Ok(());
        }
        if played {
            return self.mark_played(user, id);
        }
        self.call(agent().delete(&format!("{}/Users/{user}/PlayingItems/{id}", self.url)).query("positionTicks", &ticks.to_string())).map(|_| ())
    }

    /// 字幕的網址（Jellyfin 轉成 SRT 給播放器；影片檔外面的 .srt 和影片裡的文字字幕都可以）
    pub fn subtitle_url(&self, id: &str, index: u32) -> String {
        format!("{}/Videos/{id}/{id}/Subtitles/{index}/0/Stream.srt?api_key={}", self.url, self.key)
    }

    /// 把原檔下載到 to（先寫暫存檔、顯示進度、可以取消）；回傳幾個位元組
    pub fn download(&self, id: &str, to: &std::path::Path) -> Result<u64, String> {
        let r = self.auth(agent_long().get(&format!("{}/Videos/{id}/stream", self.url)).query("static", "true")).call().map_err(|e| describe(&e))?;
        let total = r.header("Content-Length").and_then(|n| n.parse().ok()).unwrap_or(0);
        if let Some(dir) = to.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        crate::transfer::save(r.into_reader(), total, to, true).map_err(|e| e.to_string())
    }

    /// 串流播放的網址（原檔直接傳過來，不在 NAS 上轉檔；可以跳著看）
    pub fn stream_url(&self, id: &str) -> String {
        format!("{}/Videos/{id}/stream?static=true&api_key={}", self.url, self.key)
    }

    /// 清單上的縮圖（寬 400 以內的 JPEG）；沒有時 None
    pub fn poster(&self, id: &str) -> Option<Vec<u8>> {
        let r = self.call(agent().get(&format!("{}/Items/{id}/Images/Primary", self.url)).query("fillWidth", "400").query("quality", "80")).ok()?;
        let mut buf = vec![];
        std::io::Read::read_to_end(&mut std::io::Read::take(r.into_reader(), 8 << 20), &mut buf).ok()?;
        (!buf.is_empty()).then_some(buf)
    }

    /// 時間軸預覽縮圖的第 index 張大圖（裡面排著 tile_w × tile_h 張小圖）
    pub fn trickplay_tile(&self, id: &str, tp: &Trickplay, index: u32) -> Option<Vec<u8>> {
        let url = format!("{}/Videos/{id}/Trickplay/{}/{index}.jpg", self.url, tp.width);
        let r = self.call(agent().get(&url).query("mediaSourceId", &tp.source)).ok()?;
        let mut buf = vec![];
        std::io::Read::read_to_end(&mut std::io::Read::take(r.into_reader(), 16 << 20), &mut buf).ok()?;
        (!buf.is_empty()).then_some(buf)
    }

    /// 伺服器上全部的影片（找重複用）：大小、長度、解析度、位元率、加入日期、user 看過了沒；一次讀 500 支
    pub fn all_videos(&self, user: &str) -> Result<Vec<crate::jf_dupes::JfVideo>, String> {
        let (mut out, mut start) = (vec![], 0usize);
        loop {
            let req = agent()
                .get(&format!("{}/Users/{user}/Items", self.url))
                .query("recursive", "true")
                .query("includeItemTypes", VIDEO_TYPES)
                .query("fields", "Path,MediaSources,DateCreated")
                .query("enableImageTypes", "Primary")
                .query("imageTypeLimit", "1")
                .query("startIndex", &start.to_string())
                .query("limit", "500")
                .query("enableTotalRecordCount", "true");
            let body = self.call(req)?.into_string().map_err(|e| e.to_string())?;
            let (page, total) = crate::jf_dupes::parse_videos(&body);
            // 這一頁有幾個（沒有長度的不算進 page，但要跳過）
            let got = serde_json::from_str::<Value>(&body).ok().and_then(|v| v.get("Items").and_then(Value::as_array).map(Vec::len)).unwrap_or(0);
            out.extend(page);
            start += got;
            // 讀完了（或伺服器沒給更多）
            if got == 0 || start >= total {
                return Ok(out);
            }
        }
    }

    /// 影片裡出現的人物（瀏覽視窗「依人物找」）：（編號, 名稱），依名稱排
    pub fn persons(&self, user: &str) -> Result<Vec<(String, String)>, String> {
        let req = agent().get(&format!("{}/Persons", self.url)).query("userId", user).query("limit", "500").query("enableImages", "false");
        let body = self.call(req)?.into_string().map_err(|e| e.to_string())?;
        let mut list = persons_of(&body);
        list.sort_by_key(|a| a.1.to_lowercase());
        Ok(list)
    }

    /// 影片用到的標籤、年份（瀏覽視窗「依標籤 / 年份找」）
    pub fn facets(&self, user: &str) -> Result<(Vec<String>, Vec<u32>), String> {
        let req = agent().get(&format!("{}/Items/Filters", self.url)).query("userId", user).query("includeItemTypes", VIDEO_TYPES).query("recursive", "true");
        let body = self.call(req)?.into_string().map_err(|e| e.to_string())?;
        Ok(facets_of(&body))
    }

    /// 一支影片的資訊（標題、說明、人物、標籤、年份），在瀏覽視窗修改用
    pub fn item_info(&self, user: &str, id: &str) -> Result<JfInfo, String> {
        let body = self.call(agent().get(&format!("{}/Users/{user}/Items/{id}", self.url)))?.into_string().map_err(|e| e.to_string())?;
        info_of(&body).ok_or_else(|| tr!("Jellyfin 的影片資料看不懂", "Couldn't understand Jellyfin's item data").to_string())
    }

    /// 把改好的資訊存回去：取回整筆資料、換上去再存（清空的欄位就清掉；不是演員的人物，例如導演，留著）
    pub fn set_info(&self, user: &str, id: &str, info: &JfInfo) -> Result<(), String> {
        let body = match self.call(agent().get(&format!("{}/Items/{id}", self.url))) {
            Ok(r) => r.into_string().map_err(|e| e.to_string())?,
            Err(_) => self.call(agent().get(&format!("{}/Users/{user}/Items/{id}", self.url)))?.into_string().map_err(|e| e.to_string())?,
        };
        let item = with_info(&body, info).ok_or_else(|| tr!("Jellyfin 的影片資料看不懂", "Couldn't understand Jellyfin's item data").to_string())?;
        self.call_body(agent().post(&format!("{}/Items/{id}", self.url)), &item).map(|_| ())
    }

    /// 從 Jellyfin 刪掉這支影片（伺服器上的檔案也一起刪，不會進資源回收筒）
    pub fn delete_item(&self, id: &str) -> Result<(), String> {
        match self.auth(agent().delete(&format!("{}/Items/{id}", self.url))).call() {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(401 | 403, _)) => {
                Err(tr!("Jellyfin 不允許刪除：到 Jellyfin 控制台 → 使用者，打開「允許刪除媒體」", "Jellyfin doesn't allow deleting: in the Jellyfin Dashboard → Users, turn on “Allow media deletion”")
                    .into())
            }
            Err(e) => Err(describe(&e)),
        }
    }

    /// 串流網址裡的影片編號（/Videos/{id}/stream）
    pub fn item_of_stream(url: &str) -> Option<String> {
        let rest = url.split("/Videos/").nth(1)?;
        let id = rest.split('/').next()?;
        (!id.is_empty()).then(|| id.to_string())
    }

    /// 在程式裡看完了：標成這個使用者看過（Jellyfin 的「已看過」、家人看過了沒都會更新）
    pub fn mark_played(&self, user: &str, id: &str) -> Result<(), String> {
        self.call(agent().post(&format!("{}/Users/{user}/PlayedItems/{id}", self.url))).map(|_| ())
    }

    /// Jellyfin 的使用者（編號, 名稱）：星號同步選「我的帳號」用
    pub fn users(&self) -> Result<Vec<(String, String)>, String> {
        let body = self.call(agent().get(&format!("{}/Users", self.url)))?.into_string().map_err(|e| e.to_string())?;
        Ok(users_of(&body))
    }

    /// 家人看過了沒、我的最愛：每個使用者看過的影片（小寫檔名 → [(誰, 最後一次看的日期)]），
    /// fav_user 的我的最愛（小寫檔名）
    pub fn watch_state(&self, fav_user: Option<&str>) -> Result<WatchState, String> {
        let mut st = WatchState::default();
        for (uid, name) in self.users()? {
            let url = format!("{}/Users/{uid}/Items?recursive=true&includeItemTypes=Movie,Video,MusicVideo,Episode&isPlayed=true&fields=Path&enableImages=false", self.url);
            let body = self.call(agent().get(&url))?.into_string().map_err(|e| e.to_string())?;
            for (file, date) in played_items(&body) {
                st.watched.entry(file).or_default().push((name.clone(), date));
            }
        }
        if let Some(uid) = fav_user.filter(|u| !u.is_empty()) {
            let url = format!("{}/Users/{uid}/Items?recursive=true&includeItemTypes=Movie,Video,MusicVideo,Episode&isFavorite=true&fields=Path&enableImages=false", self.url);
            let body = self.call(agent().get(&url))?.into_string().map_err(|e| e.to_string())?;
            st.favorites = played_items(&body).into_iter().map(|(f, _)| f).collect();
            st.fav_user = Some(uid.to_string());
        }
        Ok(st)
    }

    /// 把這個檔名的影片加進（或拿出）user 的我的最愛；回傳改了幾支（Jellyfin 還沒有這支時 0）
    pub fn set_favorite(&self, user: &str, file_name: &str, on: bool) -> Result<usize, String> {
        let ids = self.find(file_name)?;
        for id in &ids {
            let url = format!("{}/Users/{user}/FavoriteItems/{id}", self.url);
            self.call(if on { agent().post(&url) } else { agent().delete(&url) })?;
        }
        Ok(ids.len())
    }

    /// 排程工作做完之前，畫面右下角顯示「做時間軸預覽縮圖 45%」（每 5 秒問一次，最多 30 分鐘）
    fn watch_task(&self, key: &str, name: &str, what: &str) {
        let start = std::time::Instant::now();
        // 剛按下去時可能還沒開始
        std::thread::sleep(Duration::from_secs(2));
        while start.elapsed() < Duration::from_secs(1800) {
            let tasks = self.call(agent().get(&format!("{}/ScheduledTasks?isHidden=false", self.url))).ok().and_then(|r| r.into_string().ok()).unwrap_or_default();
            let Some(pct) = task_progress(&tasks, key) else { break };
            crate::transfer::set_stage(name, Some(format!("{what} {pct:.0}%")));
            std::thread::sleep(Duration::from_secs(5));
        }
        crate::transfer::set_stage(name, None);
    }

    /// 執行排程工作（Key 是 key 的那個）
    fn run_task(&self, key: &str) -> Result<(), String> {
        let tasks = self.call(agent().get(&format!("{}/ScheduledTasks?isHidden=false", self.url)))?.into_string().map_err(|e| e.to_string())?;
        let id = task_id(&tasks, key).ok_or_else(|| trf!("這版 Jellyfin 沒有 {key} 工作", "This Jellyfin has no {key} task"))?;
        self.call(agent().post(&format!("{}/ScheduledTasks/Running/{id}", self.url))).map(|_| ())
    }

    /// 有沒有任何一個媒體庫打開了「時間軸預覽縮圖」（Trickplay）
    fn trickplay_enabled(&self) -> bool {
        self.call(agent().get(&format!("{}/Library/VirtualFolders", self.url))).ok().and_then(|r| r.into_string().ok()).is_some_and(|libs| library_option_on(&libs, "EnableTrickplayImageExtraction"))
    }

    /// 這支影片還沒有橫幅圖（Backdrop）、縮圖（Thumb）時，用它的封面補上（首頁「繼續觀看」、「最近加入」的大圖才不會空白）
    fn fill_backdrop(&self, id: &str) {
        let Ok(item) = self.call(agent().get(&format!("{}/Items/{id}", self.url))).and_then(|r| r.into_string().map_err(|e| e.to_string())) else { return };
        let missing = missing_images(&item);
        if missing.is_empty() {
            return;
        }
        let Some(jpeg) = self.image(id, "Primary") else { return };
        for kind in missing {
            if let Err(e) = self.set_image(id, kind, &jpeg) {
                crate::info!("[Jellyfin] 設定 {kind} 圖失敗：{e}");
            }
        }
    }

    /// 下載這支影片的某種圖片（Primary 等）；沒有時 None
    fn image(&self, id: &str, kind: &str) -> Option<Vec<u8>> {
        let r = self.call(agent().get(&format!("{}/Items/{id}/Images/{kind}", self.url))).ok()?;
        let mut buf = vec![];
        std::io::Read::read_to_end(&mut std::io::Read::take(r.into_reader(), 20 << 20), &mut buf).ok()?;
        (!buf.is_empty()).then_some(buf)
    }

    /// 換掉這支影片的某種圖片（Primary 封面、Backdrop 橫幅圖、Thumb 縮圖）：圖片用 base64 送過去
    fn set_image(&self, id: &str, kind: &str, jpeg: &[u8]) -> Result<(), String> {
        let ct = if jpeg.starts_with(b"\x89PNG") { "image/png" } else { "image/jpeg" };
        let req = self.auth(agent().post(&format!("{}/Items/{id}/Images/{kind}", self.url))).set("Content-Type", ct);
        req.send_string(&crate::steps::base64(jpeg)).map(|_| ()).map_err(|e| describe(&e))
    }

    /// 把影片放進合集（沒有這個合集就新建）
    fn add_to_collection(&self, name: &str, id: &str) -> Result<(), String> {
        let req = agent().get(&format!("{}/Items", self.url)).query("recursive", "true").query("includeItemTypes", "BoxSet").query("searchTerm", name);
        let body = self.call(req)?.into_string().map_err(|e| e.to_string())?;
        match collection_id(&body, name) {
            Some(cid) => self.call(agent().post(&format!("{}/Collections/{cid}/Items", self.url)).query("ids", id)).map(|_| ()),
            None => {
                crate::info!("[Jellyfin] 新建合集「{name}」");
                self.call(agent().post(&format!("{}/Collections", self.url)).query("name", name).query("ids", id)).map(|_| ())
            }
        }
    }

    /// 請 Jellyfin 產生章節縮圖（執行「擷取章節圖片」排程工作）。回傳媒體庫有沒有打開「擷取章節圖片」（沒打開時工作不會做）
    pub fn chapter_images(&self) -> Result<bool, String> {
        let tasks = self.call(agent().get(&format!("{}/ScheduledTasks?isHidden=false", self.url)))?.into_string().map_err(|e| e.to_string())?;
        let id = task_id(&tasks, "RefreshChapterImages").ok_or_else(|| tr!("這版 Jellyfin 沒有「擷取章節圖片」工作", "This Jellyfin has no chapter image task").to_string())?;
        self.call(agent().post(&format!("{}/ScheduledTasks/Running/{id}", self.url)))?;
        let libs = self.call(agent().get(&format!("{}/Library/VirtualFolders", self.url))).ok().and_then(|r| r.into_string().ok()).unwrap_or_default();
        Ok(chapter_images_enabled(&libs))
    }

    /// 傳好之後：等 Jellyfin 掃到這支（新檔案），設好標籤、請它做章節縮圖；回傳接在結果後面的說明
    /// local：媒體庫裡那一份（依它在哪個資料夾放進合集）
    pub fn after_upload(&self, file_name: &str, extras: &Extras, ids: Vec<String>, wait: Duration, local: Option<&std::path::Path>) -> String {
        let stage = |what: &str| crate::transfer::set_stage(file_name, Some(what.to_string()));
        let ids = if ids.is_empty() {
            stage(tr!("等 Jellyfin 掃描到這支", "Waiting for Jellyfin to find it"));
            let found = self.wait_for(file_name, wait);
            crate::transfer::set_stage(file_name, None);
            match found {
                Ok(Some(id)) => vec![id],
                Ok(None) => return tr!("（Jellyfin 還在掃描，稍後就會出現）", " (Jellyfin is still scanning; it'll show up shortly)").into(),
                Err(e) => return trf!("（查不到 Jellyfin 有沒有收到：{e}）", " (couldn't check whether Jellyfin got it: {e})"),
            }
        } else {
            ids
        };
        stage(tr!("設定影片資訊、封面", "Setting details and cover"));
        let out = self.finish_upload(file_name, extras, &ids, local);
        crate::transfer::set_stage(file_name, None);
        out
    }

    fn finish_upload(&self, file_name: &str, extras: &Extras, ids: &[String], local: Option<&std::path::Path>) -> String {
        let mut notes = vec![tr!("✓ Jellyfin 已收到，可以看了", "✓ Jellyfin has it and it's ready to watch").to_string()];
        if extras.has_details() {
            let failed = ids.iter().filter_map(|id| self.set_details(id, extras).err()).next();
            notes.push(match failed {
                None => tr!("影片資訊已更新", "details updated").to_string(),
                Some(e) => {
                    crate::warn!("[Jellyfin] 設定 {file_name} 的影片資訊失敗：{e}");
                    trf!("影片資訊沒有設好：{e}", "details not set: {e}")
                }
            });
        }
        // 首頁「繼續觀看」的橫幅圖、縮圖：沒有時用封面（Jellyfin 的主要圖片）
        for id in ids {
            self.fill_backdrop(id);
        }
        if let Some(name) = local.filter(|_| self.collections).and_then(|l| collection_name(&self.root, l)) {
            notes.push(match ids.iter().try_for_each(|id| self.add_to_collection(&name, id)) {
                Ok(()) => trf!("放進合集「{name}」", "added to the “{name}” collection"),
                Err(e) => {
                    crate::warn!("[Jellyfin] 放進合集「{name}」失敗：{e}");
                    trf!("沒有放進合集：{e}", "not added to a collection: {e}")
                }
            });
        }
        // 時間軸預覽縮圖（10.9 以後）：媒體庫有打開才做，沒打開時設定裡的「一鍵修好」會打開。做的時候畫面右下角顯示進度
        if self.trickplay_enabled() && self.run_task("RefreshTrickplayImages").is_ok() {
            notes.push(tr!("正在做時間軸預覽縮圖", "making trickplay previews").into());
            let (jf, name) = (self.clone(), file_name.to_string());
            std::thread::spawn(move || jf.watch_task("RefreshTrickplayImages", &name, tr!("做時間軸預覽縮圖", "Making trickplay previews")));
        }
        if extras.chapters {
            notes.push(match self.chapter_images() {
                Ok(true) => tr!("正在做章節縮圖", "making chapter thumbnails").into(),
                Ok(false) => tr!("要做章節縮圖請在 Jellyfin 的媒體庫設定打開「擷取章節圖片」", "turn on “Extract chapter images” in the Jellyfin library settings for chapter thumbnails").into(),
                Err(e) => {
                    crate::warn!("[Jellyfin] 章節縮圖：{e}");
                    trf!("章節縮圖沒有做：{e}", "no chapter thumbnails: {e}")
                }
            });
        }
        crate::info!("[Jellyfin] {file_name}：{}", notes.join("、"));
        let notes = notes.join(tr!("；", "; "));
        trf!("（{notes}）", " ({notes})")
    }

    /// 請 Jellyfin 重新讀取這個檔名的影片（資訊與封面都換成檔案裡的）：改了封面、影片資訊時用。
    /// 只保留修改時間的改寫，媒體庫掃描不會發現，要逐支重新整理。回傳重新整理了幾支；
    /// Jellyfin 還沒有這支影片（0 支）時改成請它掃描媒體庫
    pub fn refresh_file(&self, file_name: &str) -> Result<usize, String> {
        self.refresh_items(file_name, None).map(|ids| ids.len())
    }

    /// 同 refresh_file，回傳重新整理的影片編號；local：媒體庫裡那一份（Jellyfin 還沒有時只掃描它所在的資料夾）
    pub fn refresh_items(&self, file_name: &str, local: Option<&std::path::Path>) -> Result<Vec<String>, String> {
        let ids = self.find(file_name)?;
        if ids.is_empty() {
            match local {
                Some(l) => self.scan(l).map(|_| ())?,
                None => self.refresh()?,
            }
            return Ok(ids);
        }
        for id in &ids {
            let url = format!("{}/Items/{id}/Refresh?metadataRefreshMode=Default&imageRefreshMode=FullRefresh&replaceAllMetadata=false&replaceAllImages=true", self.url);
            self.call(agent().post(&url))?;
        }
        Ok(ids)
    }

    /// 帶上 API 金鑰送出；失敗時回傳給使用者看的原因
    fn call(&self, req: ureq::Request) -> Result<ureq::Response, String> {
        self.auth(req).call().map_err(|e| describe(&e))
    }

    fn call_body(&self, req: ureq::Request, json: &str) -> Result<ureq::Response, String> {
        self.auth(req).set("Content-Type", "application/json").send_string(json).map_err(|e| describe(&e))
    }

    fn auth(&self, req: ureq::Request) -> ureq::Request {
        req.set("Authorization", &format!("MediaBrowser Token=\"{}\"", self.key)).set("X-Emby-Token", &self.key)
    }
}

/// 傳到 Jellyfin 時一起設定的：影片資訊的標籤、有沒有章節（標記）
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extras {
    pub tags: Vec<String>,
    pub chapters: bool,
    /// 影片資訊的標題、說明、類型、年份（空的不動 Jellyfin 上的）
    pub title: String,
    pub overview: String,
    pub genres: Vec<String>,
    pub year: Option<i32>,
    /// 錄影時間（UTC，Jellyfin 的格式）：設成首映日期與加入日期，依日期排序時照錄影的時間排
    pub recorded: Option<String>,
    /// 影片裡的人物（設成 Jellyfin 的演員，點一個人就列出他的影片）
    pub people: Vec<String>,
    /// 只給大人看：分級設成 R，並留著「18+」標籤（兒童帳號的家長控制可以擋掉）
    pub adult: bool,
}

impl Extras {
    /// 從影片的 ffmetadata 讀出（標題、說明、類型、年份、keywords、[CHAPTER]）
    pub fn from_ffmetadata(text: &str) -> Extras {
        let m: std::collections::HashMap<String, String> = crate::video_tags::parse_ffmetadata(text).into_iter().map(|(k, v)| (k.to_lowercase(), v.trim().to_string())).collect();
        let get = |k: &str| m.get(k).cloned().unwrap_or_default();
        let overview = if get("comment").is_empty() { get("description") } else { get("comment") };
        let year = get("date").chars().take(4).collect::<String>().parse().ok().filter(|y| (1900..=2999).contains(y));
        let (mut tags, people, adult) = crate::video_tags::split_keywords(&get("keywords"));
        if adult {
            tags.push(crate::video_tags::ADULT_TAG.into());
        }
        Extras {
            tags,
            people,
            adult,
            chapters: text.lines().any(|l| l.trim() == "[CHAPTER]"),
            title: get("title"),
            overview,
            genres: crate::video_tags::split_genres(&get("genre")),
            year,
            recorded: None,
        }
    }

    /// 有影片資訊要設到 Jellyfin
    pub fn has_details(&self) -> bool {
        !self.tags.is_empty()
            || !self.title.is_empty()
            || !self.overview.is_empty()
            || !self.genres.is_empty()
            || self.year.is_some()
            || self.recorded.is_some()
            || !self.people.is_empty()
            || self.adult
    }

    /// 讀影片（FFmpeg 讀不到時當作沒有）
    pub async fn read(ffmpeg: Option<&std::path::Path>, video: &std::path::Path) -> Extras {
        let recorded = recorded_at(video);
        let Some(ff) = ffmpeg else { return Extras { recorded, ..Default::default() } };
        let v = video.display().to_string();
        let args = ["-hide_banner", "-loglevel", "error", "-i", v.as_str(), "-f", "ffmetadata", "-"];
        let r = crate::process::run(ff, &args, Duration::from_secs(30)).await;
        let x = if r.code == 0 { Extras::from_ffmetadata(&r.stdout) } else { Extras::default() };
        Extras { recorded, ..x }
    }
}

/// 錄影時間：檔名裡的時間（Rec_2026-10-20_07-30-00、PXL_20261012_145733464、VID_20240501_103000…，當地時間），
/// 沒有時用檔案的修改時間。回傳 Jellyfin 的日期格式（UTC）
pub fn recorded_at(video: &std::path::Path) -> Option<String> {
    use chrono::TimeZone;
    let name = video.file_stem()?.to_string_lossy().into_owned();
    let from_name = name_time(&name).and_then(|t| chrono::Local.from_local_datetime(&t).earliest()).map(|t| t.with_timezone(&chrono::Utc));
    let t = from_name.or_else(|| std::fs::metadata(video).and_then(|m| m.modified()).ok().map(chrono::DateTime::<chrono::Utc>::from))?;
    Some(t.format("%Y-%m-%dT%H:%M:%S.0000000Z").to_string())
}

/// 檔名裡的日期時間：2026-10-20_07-30-00、20261012_145733、2026-10-20 07.30.00 這類寫法
fn name_time(name: &str) -> Option<chrono::NaiveDateTime> {
    let digits: Vec<(usize, String)> = {
        // 連續數字一段一段取出（位置, 數字）
        let mut out = vec![];
        let mut cur = String::new();
        let mut at = 0;
        for (i, c) in name.char_indices() {
            if c.is_ascii_digit() {
                if cur.is_empty() {
                    at = i;
                }
                cur.push(c);
            } else if !cur.is_empty() {
                out.push((at, std::mem::take(&mut cur)));
            }
        }
        if !cur.is_empty() {
            out.push((at, cur));
        }
        out
    };
    let nums: Vec<&str> = digits.iter().map(|(_, d)| d.as_str()).collect();
    for i in 0..nums.len() {
        // 20261012 + 145733（後面可能多毫秒）
        if nums[i].len() == 8 && nums.get(i + 1).is_some_and(|t| t.len() >= 6) {
            let s = format!("{}{}", nums[i], &nums[i + 1][..6]);
            if let Ok(t) = chrono::NaiveDateTime::parse_from_str(&s, "%Y%m%d%H%M%S") {
                return Some(t);
            }
        }
        // 2026 10 20 07 30 00
        if nums[i].len() == 4 && nums.len() >= i + 6 && nums[i + 1..i + 6].iter().all(|n| n.len() == 2) {
            let s = nums[i..i + 6].join("-");
            if let Ok(t) = chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d-%H-%M-%S") {
                return Some(t);
            }
        }
    }
    None
}

impl Extras {
    /// 錄影時間的年份（影片資訊沒填年份時用）
    fn recorded_year(&self) -> Option<i32> {
        self.recorded.as_deref().and_then(|r| r.get(..4)).and_then(|y| y.parse().ok())
    }
}

/// 整筆影片資料換上影片資訊：標題、說明、類型、年份、標籤（空的照舊；其他欄位照原樣送回去，不然 Jellyfin 會清掉）
pub fn with_details(item_json: &str, x: &Extras) -> Option<String> {
    let mut v: Value = serde_json::from_str(item_json).ok()?;
    let o = v.as_object_mut()?;
    if !x.title.is_empty() {
        o.insert("Name".into(), Value::from(x.title.clone()));
    }
    if !x.overview.is_empty() {
        o.insert("Overview".into(), Value::from(x.overview.clone()));
    }
    if !x.genres.is_empty() {
        o.insert("Genres".into(), Value::from(x.genres.clone()));
    }
    if let Some(y) = x.year.or_else(|| x.recorded_year().filter(|_| o.get("ProductionYear").is_none_or(Value::is_null))) {
        o.insert("ProductionYear".into(), Value::from(y));
    }
    if !x.people.is_empty() {
        o.insert("People".into(), x.people.iter().map(|n| serde_json::json!({ "Name": n, "Type": "Actor" })).collect());
    }
    // 只給大人看：R 級（兒童帳號設最高分級就看不到）；拿掉時清掉我們設的 R
    if x.adult {
        o.insert("OfficialRating".into(), Value::from("R"));
    } else if o.get("OfficialRating").and_then(Value::as_str) == Some("R") {
        o.insert("OfficialRating".into(), Value::from(""));
    }
    if let Some(r) = &x.recorded {
        o.insert("PremiereDate".into(), Value::from(r.clone()));
        o.insert("DateCreated".into(), Value::from(r.clone()));
    }
    if !x.tags.is_empty() {
        o.insert("Tags".into(), Value::from(x.tags.clone()));
    }
    serde_json::to_string(&v).ok()
}

/// 排程工作清單裡 Key 是 key 的工作編號
pub fn task_id(json: &str, key: &str) -> Option<String> {
    let v: Value = serde_json::from_str(json).ok()?;
    v.as_array()?.iter().find(|t| t.get("Key").and_then(Value::as_str) == Some(key)).and_then(|t| t.get("Id").and_then(Value::as_str)).map(String::from)
}

/// 有沒有任何一個媒體庫打開了「擷取章節圖片」
/// 排程工作正在做時的進度（0–100）；沒在做時 None
pub fn task_progress(json: &str, key: &str) -> Option<f64> {
    let v: Value = serde_json::from_str(json).ok()?;
    let t = v.as_array()?.iter().find(|t| t.get("Key").and_then(Value::as_str) == Some(key))?;
    (t.get("State").and_then(Value::as_str) == Some("Running")).then(|| t.get("CurrentProgressPercentage").and_then(Value::as_f64).unwrap_or(0.0))
}

pub fn chapter_images_enabled(json: &str) -> bool {
    serde_json::from_str::<Value>(json)
        .ok()
        .and_then(|v| v.as_array().map(|libs| libs.iter().any(|l| l.pointer("/LibraryOptions/EnableChapterImageExtraction").and_then(Value::as_bool) == Some(true))))
        .unwrap_or(false)
}

/// 傳到媒體庫時字幕檔的名字：中文字幕加上語言（同名.zh-TW.srt），Jellyfin 的字幕選單才會顯示「中文」而不是「未知」；
/// 已經有語言的（同名.en.srt）照舊
pub fn library_name(file: &std::path::Path) -> String {
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let lower = name.to_lowercase();
    let Some(stem) = lower.strip_suffix(".srt").map(|_| &name[..name.len() - 4]) else { return name };
    if stem.rsplit_once('.').is_some_and(|(_, lang)| (2..=3).contains(&lang.len()) || lang.contains('-')) {
        return name;
    }
    let cjk = std::fs::read_to_string(file).is_ok_and(|t| t.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)));
    if cjk {
        format!("{stem}.zh-TW.srt")
    } else {
        name
    }
}

/// 從 Jellyfin 最近加入的影片路徑推出媒體庫資料夾 local_root 在伺服器上的路徑：伺服器路徑的後面幾層
/// （例如 other/Rec_1.mp4）在 local_root 底下找得到，前面那段就是。後面幾層取最長的那個；都對不上時 None
pub fn server_root_from(items_json: &str, local_root: &str) -> Option<String> {
    let v: Value = serde_json::from_str(items_json).ok()?;
    let local = std::path::Path::new(local_root.trim());
    for path in v.get("Items")?.as_array()?.iter().filter_map(|i| i.get("Path").and_then(Value::as_str)).take(100) {
        // 每個分隔符號的位置：後面 k 層從倒數第 k 個分隔符號之後開始
        let seps: Vec<usize> = path.char_indices().filter(|(_, c)| *c == '/' || *c == '\\').map(|(i, _)| i).collect();
        for k in (1..=seps.len()).rev() {
            let at = seps[seps.len() - k];
            let rel = &path[at + 1..];
            if rel.is_empty() || !local.join(rel.replace(['/', '\\'], std::path::MAIN_SEPARATOR_STR)).is_file() {
                continue;
            }
            let root = &path[..at];
            return Some(if root.is_empty() { path[..1].to_string() } else { root.to_string() });
        }
    }
    None
}

/// 這台電腦上媒體庫裡的資料夾 local_dir，在伺服器上的路徑（server_root 是 local_root 在伺服器上的路徑）；不在媒體庫裡時 None
pub fn server_path(server_root: &str, local_root: &str, local_dir: &std::path::Path) -> Option<String> {
    let norm = |s: &str| s.replace('\\', "/").trim_end_matches('/').to_string();
    let (root, dir) = (norm(local_root.trim()), norm(&local_dir.display().to_string()));
    if root.is_empty() || dir.len() < root.len() || !dir.is_char_boundary(root.len()) || !dir[..root.len()].eq_ignore_ascii_case(&root) {
        return None;
    }
    let rest = &dir[root.len()..];
    if !(rest.is_empty() || rest.starts_with('/')) {
        return None;
    }
    // 伺服器是 Windows（路徑用 \）時照它的寫法
    let sep = if server_root.contains('\\') && !server_root.contains('/') { '\\' } else { '/' };
    let mut out = server_root.trim_end_matches(['/', '\\']).to_string();
    for part in rest.split('/').filter(|p| !p.is_empty()) {
        out.push(sep);
        out.push_str(part);
    }
    if out.is_empty() {
        out.push(sep);
    }
    Some(out)
}

/// Jellyfin 上的一個東西：資料夾（媒體庫、資料夾、合集）或影片
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JfKind {
    Folder,
    Video,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JfItem {
    pub id: String,
    pub name: String,
    pub kind: JfKind,
    /// 影片的檔名（播放時判斷格式用）；資料夾是空的
    pub file_name: String,
    /// 秒；資料夾 0
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub has_audio: bool,
    /// YYYY-MM-DD（首映日期，沒有時加入日期）
    pub date: String,
    /// 這個使用者看過了
    pub played: bool,
    /// 資料夾裡有幾個
    pub children: u32,
    pub has_image: bool,
    /// 這個使用者上次看到哪裡（秒；沒看過或看完了是 0）
    pub resume: f64,
    /// 文字字幕（圖片字幕播放器疊不上去，不列）
    pub subs: Vec<JfSub>,
    /// 章節（開始的秒數, 名稱）
    pub chapters: Vec<(f64, String)>,
    /// Jellyfin 做好的時間軸預覽縮圖（沒有時 None）
    pub trickplay: Option<Trickplay>,
}

/// 時間軸預覽縮圖：每 interval 秒一張小圖（width × height），tile_w × tile_h 張排成一張大圖
#[derive(Debug, Clone, PartialEq)]
pub struct Trickplay {
    pub source: String,
    pub width: u32,
    pub height: u32,
    pub tile_w: u32,
    pub tile_h: u32,
    pub count: u32,
    pub interval: f64,
}

impl Trickplay {
    /// t 秒的小圖在哪一張大圖、大圖裡的哪一格（UV，0～1）
    pub fn at(&self, t: f64) -> (u32, [f32; 4]) {
        let k = ((t.max(0.0) / self.interval).floor() as u32).min(self.count.saturating_sub(1));
        let per = (self.tile_w * self.tile_h).max(1);
        let (index, pos) = (k / per, k % per);
        let (col, row) = (pos % self.tile_w.max(1), pos / self.tile_w.max(1));
        let (w, h) = (1.0 / self.tile_w.max(1) as f32, 1.0 / self.tile_h.max(1) as f32);
        (index, [col as f32 * w, row as f32 * h, (col + 1) as f32 * w, (row + 1) as f32 * h])
    }
}

/// 瀏覽視窗依什麼找：人物（編號, 名稱）、標籤、年份
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JfFacet {
    Person(String, String),
    Tag(String),
    Year(u32),
}

impl JfFacet {
    pub fn label(&self) -> String {
        match self {
            JfFacet::Person(_, n) => trf!("人物：{n}", "Person: {n}"),
            JfFacet::Tag(t) => trf!("標籤：{t}", "Tag: {t}"),
            JfFacet::Year(y) => trf!("{y} 年", "Year {y}"),
        }
    }
}

/// 在瀏覽視窗修改的影片資訊
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JfInfo {
    pub title: String,
    pub overview: String,
    pub people: Vec<String>,
    pub tags: Vec<String>,
    pub year: Option<i32>,
}

/// 一個字幕：Jellyfin 的串流編號、顯示的名稱（語言）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JfSub {
    pub index: u32,
    pub title: String,
}

/// 影片類型（Jellyfin 的 includeItemTypes）
const VIDEO_TYPES: &str = "Movie,Video,MusicVideo,Episode";

/// 瀏覽的排序
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JfSort {
    /// 錄影時間新的在前（沒有時用加入的時間）
    #[default]
    Newest,
    Oldest,
    /// 最近放進 Jellyfin 的在前
    Added,
    Name,
}

impl JfSort {
    pub const ALL: [JfSort; 4] = [JfSort::Newest, JfSort::Oldest, JfSort::Added, JfSort::Name];

    pub fn label(self) -> &'static str {
        match self {
            JfSort::Newest => tr!("最新的在前", "Newest first"),
            JfSort::Oldest => tr!("最舊的在前", "Oldest first"),
            JfSort::Added => tr!("最近加入的在前", "Recently added"),
            JfSort::Name => tr!("依名稱", "By name"),
        }
    }

    fn query(self) -> (&'static str, &'static str) {
        match self {
            JfSort::Newest => ("PremiereDate,DateCreated,SortName", "Descending,Descending,Ascending"),
            JfSort::Oldest => ("PremiereDate,DateCreated,SortName", "Ascending,Ascending,Ascending"),
            JfSort::Added => ("DateCreated,SortName", "Descending,Ascending"),
            JfSort::Name => ("SortName", "Ascending"),
        }
    }
}

/// 瀏覽的篩選
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JfFilter {
    #[default]
    All,
    Unplayed,
    Resumable,
    Favorite,
}

impl JfFilter {
    pub const ALL: [JfFilter; 4] = [JfFilter::All, JfFilter::Unplayed, JfFilter::Resumable, JfFilter::Favorite];

    pub fn label(self) -> &'static str {
        match self {
            JfFilter::All => tr!("全部", "All"),
            JfFilter::Unplayed => tr!("還沒看過的", "Not watched"),
            JfFilter::Resumable => tr!("看到一半的", "Partly watched"),
            JfFilter::Favorite => tr!("我的最愛", "Favorites"),
        }
    }

    fn query(self) -> Option<&'static str> {
        match self {
            JfFilter::All => None,
            JfFilter::Unplayed => Some("IsUnplayed"),
            JfFilter::Resumable => Some("IsResumable"),
            JfFilter::Favorite => Some("IsFavorite"),
        }
    }
}

/// 把 Jellyfin 的網址參數 api_key 遮掉（寫進記錄檔時用）
pub fn redact(text: &str) -> String {
    match text.find("api_key=") {
        Some(i) => {
            let end = text[i..].find('&').map_or(text.len(), |j| i + j);
            format!("{}api_key=…{}", &text[..i], &text[end..])
        }
        None => text.to_string(),
    }
}

/// /Items、/Views 的回應：（東西, 全部幾個）。照片、音樂等不是影片也不是資料夾的略過。
/// /Items/Latest 回傳的是陣列（沒有外面那一層）
pub fn parse_items(json: &str) -> (Vec<JfItem>, usize) {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return (vec![], 0) };
    let list = if v.is_array() { Some(&v) } else { v.get("Items") };
    let total = v.get("TotalRecordCount").and_then(Value::as_u64).unwrap_or_else(|| list.and_then(Value::as_array).map_or(0, |a| a.len() as u64)) as usize;
    let items = list
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|i| {
                    let kind = match i.get("Type").and_then(Value::as_str).unwrap_or("") {
                        "Video" | "Movie" | "Episode" | "MusicVideo" => JfKind::Video,
                        _ if i.get("IsFolder").and_then(Value::as_bool) == Some(true) => JfKind::Folder,
                        _ => return None,
                    };
                    let streams = i.get("MediaStreams").and_then(Value::as_array).cloned().unwrap_or_default();
                    let video = streams.iter().find(|s| s.get("Type").and_then(Value::as_str) == Some("Video") && s.get("IsExternal").and_then(Value::as_bool) != Some(true));
                    let num = |s: Option<&Value>, k: &str| s.and_then(|s| s.get(k)).and_then(Value::as_f64).unwrap_or(0.0);
                    let date = ["PremiereDate", "DateCreated"].iter().find_map(|k| i.get(*k).and_then(Value::as_str)).and_then(|d| d.get(..10)).unwrap_or("").to_string();
                    Some(JfItem {
                        id: i.get("Id")?.as_str()?.to_string(),
                        name: i.get("Name").and_then(Value::as_str).unwrap_or("?").to_string(),
                        kind,
                        file_name: i.get("Path").and_then(Value::as_str).and_then(|p| p.rsplit(['/', '\\']).next()).unwrap_or("").to_string(),
                        duration: i.get("RunTimeTicks").and_then(Value::as_f64).unwrap_or(0.0) / 1e7,
                        width: num(video, "Width") as u32,
                        height: num(video, "Height") as u32,
                        fps: [num(video, "RealFrameRate"), num(video, "AverageFrameRate")].into_iter().find(|f| *f > 0.0).unwrap_or(30.0),
                        has_audio: streams.iter().any(|s| s.get("Type").and_then(Value::as_str) == Some("Audio")),
                        date,
                        played: i.pointer("/UserData/Played").and_then(Value::as_bool) == Some(true),
                        children: i.get("ChildCount").and_then(Value::as_u64).unwrap_or(0) as u32,
                        has_image: i.pointer("/ImageTags/Primary").is_some(),
                        resume: i.pointer("/UserData/PlaybackPositionTicks").and_then(Value::as_f64).unwrap_or(0.0) / 1e7,
                        subs: streams
                            .iter()
                            .filter(|s| s.get("Type").and_then(Value::as_str) == Some("Subtitle") && s.get("IsTextSubtitleStream").and_then(Value::as_bool) != Some(false))
                            .filter_map(|s| {
                                let title = ["DisplayTitle", "Title", "Language"].iter().find_map(|k| s.get(*k).and_then(Value::as_str)).unwrap_or("").to_string();
                                Some(JfSub { index: s.get("Index")?.as_u64()? as u32, title })
                            })
                            .collect(),
                        chapters: i
                            .get("Chapters")
                            .and_then(Value::as_array)
                            .map(|a| {
                                a.iter().map(|c| (c.get("StartPositionTicks").and_then(Value::as_f64).unwrap_or(0.0) / 1e7, c.get("Name").and_then(Value::as_str).unwrap_or("").to_string())).collect()
                            })
                            .unwrap_or_default(),
                        trickplay: i.get("Trickplay").and_then(trickplay_of),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    (items, total)
}

/// 項目的 Trickplay：{ 媒體來源: { 寬度: { Width, Height, TileWidth, TileHeight, ThumbnailCount, Interval(毫秒) } } }；
/// 有好幾種寬度時挑 200 以上最小的（預覽框不大，下載快）
fn trickplay_of(v: &Value) -> Option<Trickplay> {
    let (source, sizes) = v.as_object()?.iter().next()?;
    let mut list: Vec<Trickplay> = sizes
        .as_object()?
        .values()
        .filter_map(|t| {
            let n = |k: &str| t.get(k).and_then(Value::as_u64).unwrap_or(0) as u32;
            let tp = Trickplay {
                source: source.clone(),
                width: n("Width"),
                height: n("Height"),
                tile_w: n("TileWidth"),
                tile_h: n("TileHeight"),
                count: n("ThumbnailCount"),
                interval: n("Interval") as f64 / 1000.0,
            };
            (tp.width > 0 && tp.height > 0 && tp.tile_w > 0 && tp.tile_h > 0 && tp.count > 0 && tp.interval > 0.0).then_some(tp)
        })
        .collect();
    list.sort_by_key(|t| (t.width < 200, t.width));
    list.into_iter().next()
}

/// /Persons 的回應：（編號, 名稱）
fn persons_of(json: &str) -> Vec<(String, String)> {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return vec![] };
    v.get("Items")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|p| Some((p.get("Id")?.as_str()?.to_string(), p.get("Name")?.as_str()?.trim().to_string()))).filter(|(_, n)| !n.is_empty()).collect())
        .unwrap_or_default()
}

/// /Items/Filters 的回應：（標籤, 年份新的在前）
fn facets_of(json: &str) -> (Vec<String>, Vec<u32>) {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return (vec![], vec![]) };
    let mut tags: Vec<String> = v.get("Tags").and_then(Value::as_array).map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect()).unwrap_or_default();
    tags.sort_by_key(|t| t.to_lowercase());
    let mut years: Vec<u32> = v.get("Years").and_then(Value::as_array).map(|a| a.iter().filter_map(|y| y.as_u64().map(|y| y as u32)).collect()).unwrap_or_default();
    years.sort_unstable_by(|a, b| b.cmp(a));
    (tags, years)
}

/// 影片的資訊（人物只算演員：我們設的人物都是演員）
fn info_of(json: &str) -> Option<JfInfo> {
    let v: Value = serde_json::from_str(json).ok()?;
    let strs = |k: &str| v.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect()).unwrap_or_default();
    Some(JfInfo {
        title: v.get("Name").and_then(Value::as_str).unwrap_or("").to_string(),
        overview: v.get("Overview").and_then(Value::as_str).unwrap_or("").to_string(),
        people: v
            .get("People")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter(|p| p.get("Type").and_then(Value::as_str).is_none_or(|t| t == "Actor")).filter_map(|p| p.get("Name")?.as_str().map(String::from)).collect())
            .unwrap_or_default(),
        tags: strs("Tags"),
        year: v.get("ProductionYear").and_then(Value::as_i64).map(|y| y as i32),
    })
}

/// 把 JfInfo 換進整筆資料（POST /Items/{id} 用）：標題空的時不改
pub fn with_info(item_json: &str, x: &JfInfo) -> Option<String> {
    let mut v: Value = serde_json::from_str(item_json).ok()?;
    let o = v.as_object_mut()?;
    if !x.title.trim().is_empty() {
        o.insert("Name".into(), Value::from(x.title.trim()));
    }
    o.insert("Overview".into(), Value::from(x.overview.trim()));
    o.insert("Tags".into(), Value::from(x.tags.clone()));
    let mut people: Vec<Value> =
        o.get("People").and_then(Value::as_array).map(|a| a.iter().filter(|p| p.get("Type").and_then(Value::as_str).is_some_and(|t| t != "Actor")).cloned().collect()).unwrap_or_default();
    people.extend(x.people.iter().map(|n| serde_json::json!({ "Name": n, "Type": "Actor" })));
    o.insert("People".into(), Value::from(people));
    o.insert("ProductionYear".into(), x.year.map_or(Value::Null, Value::from));
    serde_json::to_string(&v).ok()
}

/// 家人看過了沒、我的最愛（Jellyfin::watch_state）
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WatchState {
    /// 小寫檔名 → [(誰, 最後一次看的日期 YYYY-MM-DD)]
    pub watched: std::collections::HashMap<String, Vec<(String, String)>>,
    /// 我的最愛（小寫檔名）
    pub favorites: std::collections::HashSet<String>,
    /// 我的最愛是哪個使用者的
    pub fav_user: Option<String>,
}

/// /Users 的回應：（編號, 名稱）
fn users_of(json: &str) -> Vec<(String, String)> {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return vec![] };
    v.as_array().map(|a| a.iter().filter_map(|u| Some((u.get("Id")?.as_str()?.to_string(), u.get("Name").and_then(Value::as_str).unwrap_or("?").to_string()))).collect()).unwrap_or_default()
}

/// /Items 的回應：每支影片的（小寫檔名, 最後一次看的日期 YYYY-MM-DD；沒有時空的）
fn played_items(json: &str) -> Vec<(String, String)> {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return vec![] };
    v.get("Items")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|i| {
                    let path = i.get("Path")?.as_str()?;
                    let name = path.rsplit(['/', '\\']).next()?.to_lowercase();
                    let date = i.pointer("/UserData/LastPlayedDate").and_then(Value::as_str).and_then(|d| d.get(..10)).unwrap_or("").to_string();
                    Some((name, date))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 上次同步時 Jellyfin 我的最愛裡的檔名（小寫）：比對出新加的、拿掉的，本機的星號跟著改。存在設定資料夾的 jellyfin-favorites.json
static FAVS: std::sync::Mutex<Option<(Option<std::path::PathBuf>, std::collections::BTreeSet<String>)>> = std::sync::Mutex::new(None);

pub fn init_favorites(file: std::path::PathBuf) {
    let mut g = FAVS.lock().unwrap();
    if g.as_ref().is_some_and(|(f, _)| f.is_some()) {
        return;
    }
    let mut set: std::collections::BTreeSet<String> = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    set.extend(g.take().map(|(_, s)| s).unwrap_or_default());
    *g = Some((Some(file), set));
}

/// 和上次比：(新加進我的最愛的, 拿掉的)，並記下這次的
pub fn favorites_changed(now: &std::collections::HashSet<String>) -> (Vec<String>, Vec<String>) {
    let mut g = FAVS.lock().unwrap();
    let (file, prev) = g.get_or_insert_with(|| (None, Default::default()));
    let added: Vec<String> = now.iter().filter(|n| !prev.contains(*n)).cloned().collect();
    let removed: Vec<String> = prev.iter().filter(|n| !now.contains(*n)).cloned().collect();
    if !added.is_empty() || !removed.is_empty() {
        *prev = now.iter().cloned().collect();
        if let Some(f) = file.as_ref() {
            let _ = std::fs::write(f, serde_json::to_string(prev).unwrap_or_default());
        }
    }
    (added, removed)
}

/// 本機改了星號、也改到 Jellyfin 之後記下（下次比對才不會又改回來）
pub fn favorite_set(name: &str, on: bool) {
    let mut g = FAVS.lock().unwrap();
    let (file, prev) = g.get_or_insert_with(|| (None, Default::default()));
    let name = name.to_lowercase();
    let changed = if on { prev.insert(name) } else { prev.remove(&name) };
    if changed {
        if let Some(f) = file.as_ref() {
            let _ = std::fs::write(f, serde_json::to_string(prev).unwrap_or_default());
        }
    }
}

/// 媒體庫 l 的資料夾有沒有包含 root（伺服器上的路徑；/ 和 \\ 都算、不分大小寫），或在 root 底下
fn covers(l: &Value, root: &str) -> bool {
    let norm = |p: &str| p.replace('\\', "/").trim_end_matches('/').to_lowercase();
    let root = norm(root);
    let under = |a: &str, b: &str| a == b || a.strip_prefix(b).is_some_and(|r| r.starts_with('/')) || b.is_empty();
    l.get("Locations").and_then(Value::as_array).is_some_and(|a| a.iter().filter_map(Value::as_str).map(norm).any(|loc| under(&root, &loc) || under(&loc, &root)))
}

/// 一鍵修好要打開的媒體庫選項：即時監控、擷取章節圖片（含掃描時）、時間軸預覽縮圖（含掃描時）
const GOOD_OPTIONS: [&str; 5] =
    ["EnableRealtimeMonitor", "EnableChapterImageExtraction", "ExtractChapterImagesDuringLibraryScan", "EnableTrickplayImageExtraction", "ExtractTrickplayImagesDuringLibraryScan"];

/// 媒體庫選項打開 GOOD_OPTIONS（其他照原樣）
fn with_good_options(opts: &Value) -> Value {
    let mut o = if opts.is_object() { opts.clone() } else { serde_json::json!({}) };
    if let Some(m) = o.as_object_mut() {
        for k in GOOD_OPTIONS {
            m.insert(k.into(), Value::Bool(true));
        }
    }
    o
}

/// 有沒有任何一個媒體庫打開了這個選項
fn library_option_on(vf_json: &str, key: &str) -> bool {
    serde_json::from_str::<Value>(vf_json)
        .ok()
        .and_then(|v| v.as_array().map(|libs| libs.iter().any(|l| l.pointer(&format!("/LibraryOptions/{key}")).and_then(Value::as_bool) == Some(true))))
        .unwrap_or(false)
}

/// /Library/VirtualFolders 的回應裡，包含 server_root 的媒體庫設定對不對：類型是不是「家庭影片和照片」、
/// 即時監控、章節圖片、時間軸預覽縮圖有沒有打開。server_root 不知道時（媒體庫還沒有影片）看不出來，None
pub fn library_check(vf_json: &str, server_root: Option<&str>) -> Option<LibraryCheck> {
    let root = server_root?;
    let v: Value = serde_json::from_str(vf_json).ok()?;
    let libs: Vec<&Value> = v.as_array()?.iter().filter(|l| covers(l, root)).collect();
    if libs.is_empty() {
        return None;
    }
    let name = |l: &Value| l.get("Name").and_then(Value::as_str).unwrap_or("?").to_string();
    let home: Vec<&Value> = libs.iter().copied().filter(|l| l.get("CollectionType").and_then(Value::as_str) == Some("homevideos")).collect();
    if home.is_empty() {
        let notes: Vec<String> = libs
            .iter()
            .map(|l| {
                let kind = match l.get("CollectionType").and_then(Value::as_str).unwrap_or("") {
                    "movies" => tr!("電影", "Movies"),
                    "tvshows" => tr!("節目", "Shows"),
                    "musicvideos" => tr!("音樂影片", "Music Videos"),
                    "music" => tr!("音樂", "Music"),
                    "books" => tr!("書籍", "Books"),
                    _ => tr!("混合內容", "Mixed content"),
                };
                trf!("媒體庫「{}」的類型是「{kind}」", "Library “{}” is set to “{kind}”", name(l))
            })
            .collect();
        let note = format!(
            "{}{}",
            notes.join(tr!("；", "; ")),
            tr!(
                "：錄影會被當成電影或影集去網路上找資訊，可能配錯封面、名稱。按「一鍵修好」新建一個「家庭影片和照片」媒體庫",
                ". Recordings get matched like films or episodes and may get the wrong cover or title. Click “Fix it” to create a Home Videos and Photos library"
            )
        );
        return Some(LibraryCheck { note, ok: false });
    }
    // 舊版沒有的選項（例如 10.9 以前沒有時間軸預覽縮圖）不算
    let labels = [
        ("EnableRealtimeMonitor", tr!("即時監控", "real-time monitoring")),
        ("EnableChapterImageExtraction", tr!("擷取章節圖片", "chapter images")),
        ("EnableTrickplayImageExtraction", tr!("時間軸預覽縮圖", "trickplay previews")),
    ];
    let off: Vec<&str> = labels.iter().filter(|(k, _)| home.iter().any(|l| l.pointer(&format!("/LibraryOptions/{k}")).and_then(Value::as_bool) == Some(false))).map(|(_, t)| *t).collect();
    let names = home.iter().map(|l| name(l)).collect::<Vec<_>>().join("、");
    if off.is_empty() {
        return Some(LibraryCheck { note: trf!("媒體庫「{names}」是家庭影片和照片 ✓", "Library “{names}” is Home Videos and Photos ✓"), ok: true });
    }
    let off = off.join(tr!("、", ", "));
    Some(LibraryCheck {
        note: trf!("媒體庫「{names}」是家庭影片和照片，但{off}沒打開：按「一鍵修好」打開", "Library “{names}” is Home Videos and Photos, but {off} is off. Click “Fix it” to turn it on"),
        ok: false,
    })
}

/// 依媒體庫裡的位置取合集名稱：Y:\family\2024\a.mp4 → 「family 2024」；直接放在最上層的不放進合集
pub fn collection_name(root: &str, local: &std::path::Path) -> Option<String> {
    let norm = |s: &str| s.replace('\\', "/").trim_end_matches('/').to_string();
    // 用字串拆（/ 和 \\ 都算），不靠 Path::parent：測試在 Linux 上跑也看得懂 Windows 路徑
    let (root, full) = (norm(root.trim()), norm(&local.display().to_string()));
    let dir = full.rsplit_once('/')?.0.to_string();
    if root.is_empty() || dir.len() <= root.len() || !dir.is_char_boundary(root.len()) || !dir[..root.len()].eq_ignore_ascii_case(&root) {
        return None;
    }
    let rest = &dir[root.len()..];
    if !rest.starts_with('/') {
        return None;
    }
    let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// /Items（合集）的回應裡名稱是 name 的那一個（不分大小寫）
fn collection_id(json: &str, name: &str) -> Option<String> {
    let v: Value = serde_json::from_str(json).ok()?;
    v.get("Items")?.as_array()?.iter().find(|i| i.get("Name").and_then(Value::as_str).is_some_and(|n| n.eq_ignore_ascii_case(name))).and_then(|i| i.get("Id").and_then(Value::as_str)).map(String::from)
}

/// 影片資料裡還沒有的圖片：Backdrop（BackdropImageTags 是空的）、Thumb（ImageTags 沒有 Thumb）；有封面時才補
fn missing_images(item_json: &str) -> Vec<&'static str> {
    let Ok(v) = serde_json::from_str::<Value>(item_json) else { return vec![] };
    if v.pointer("/ImageTags/Primary").is_none() {
        return vec![];
    }
    let mut out = vec![];
    if v.get("BackdropImageTags").and_then(Value::as_array).is_none_or(|a| a.is_empty()) {
        out.push("Backdrop");
    }
    if v.pointer("/ImageTags/Thumb").is_none() {
        out.push("Thumb");
    }
    out
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

/// 改了影片的哪個部分：決定媒體庫裡那一份要不要整支重傳
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    /// 畫面變了（旋轉）：整支換掉
    Video,
    /// 換了封面：Jellyfin 已經有這支時只上傳封面圖
    Cover,
    /// 改了影片資訊：Jellyfin 已經有這支時只透過它改
    Details,
}

/// 改了影片（封面、影片資訊、畫面）之後同步到 Jellyfin：
/// - 這支就在「複製」的資料夾裡（直接改媒體庫裡的檔案）：請 Jellyfin 重新讀取
/// - 改的是本機的原檔，而「複製」的資料夾裡有同名的一份：只換封面、影片資訊且 Jellyfin 已經有這支時，
///   只透過 Jellyfin 更新（不重傳整支影片，NAS 上可能好幾 GB）；其他情況先用改好的檔案換掉那一份，再請 Jellyfin 重新讀取
///
/// cover：Edit::Cover 時改好的封面圖（JPEG）。都不是（或沒設定）時回傳 None；有做事時回傳給使用者看的結果
pub fn sync_edited(jf: Option<&Jellyfin>, copy_to: &str, video: &std::path::Path, extras: &Extras, edit: Edit, cover: Option<&[u8]>) -> Option<Result<String, String>> {
    let name = video.file_name()?.to_string_lossy().into_owned();
    let copy_to = copy_to.trim();
    if copy_to.is_empty() {
        return None;
    }
    let mut copied = false;
    let mut target = video.to_path_buf();
    if !in_library(copy_to, video) {
        // 媒體庫（最上層或第一層子資料夾）裡同名的那一份；本機改過了大小會不同，只比對名稱，有好幾份時不知道是哪一份就不動
        let found = find_in_library(copy_to, &name, None);
        let [dest] = found.as_slice() else {
            if found.len() > 1 {
                crate::info!("[Jellyfin] 媒體庫裡有 {} 個 {name}，不知道要更新哪一份", found.len());
            }
            return None;
        };
        if let Some(msg) = jf.and_then(|jf| update_in_place(jf, &name, edit, extras, cover)) {
            // 兩邊的大小不一樣了：記下來，「已在媒體庫」、跟著改名、刪除照樣認得
            if let (Ok(a), Ok(b)) = (std::fs::metadata(video), std::fs::metadata(dest)) {
                remember_link(&name, a.len(), b.len());
            }
            return Some(Ok(msg));
        }
        if let Err(e) = crate::transfer::copy(video, dest) {
            crate::warn!("[Jellyfin] 更新 {} 失敗：{e}", dest.display());
            return Some(Err(trf!("沒有更新「複製」資料夾裡的那一份：{e}", "Couldn't update the copy in the “Copy” folder: {e}")));
        }
        crate::info!("[Jellyfin] 已用改好的檔案換掉 {}", dest.display());
        copied = true;
        target = dest.clone();
    }
    let Some(jf) = jf else {
        return copied.then(|| Ok(tr!("也更新了「複製」資料夾裡的那一份", "The copy in the “Copy” folder was updated too").to_string()));
    };
    let done = if copied { tr!("已更新「複製」資料夾裡的那一份，", "Updated the copy in the “Copy” folder; ") } else { "" };
    Some(match jf.refresh_items(&name, Some(&target)) {
        Ok(ids) if ids.is_empty() => Ok(format!("{done}{}", tr!("Jellyfin 還沒有這支影片，已請它掃描媒體庫", "Jellyfin doesn't have this video yet, so it was asked to scan"))),
        Ok(ids) => {
            let more = jf.after_upload(&name, extras, ids, Duration::ZERO, Some(&target));
            Ok(format!("{done}{}{more}", tr!("已請 Jellyfin 重新讀取這支影片", "Jellyfin was asked to reload this video")))
        }
        Err(e) => {
            crate::warn!("[Jellyfin] 重新整理 {name} 失敗：{e}");
            Err(format!("{done}{}", trf!("但沒有通知到 Jellyfin：{e}", "but Jellyfin couldn't be notified: {e}")))
        }
    })
}

/// 只透過 Jellyfin 更新封面或影片資訊（媒體庫裡的檔案不動）：Jellyfin 還沒有這支、或更新失敗時 None（改成整支重傳）
fn update_in_place(jf: &Jellyfin, name: &str, edit: Edit, extras: &Extras, cover: Option<&[u8]>) -> Option<String> {
    let ids = match edit {
        Edit::Video => return None,
        // 影片資訊全部清空時 Jellyfin 上的不會跟著清：整支重傳讓它重讀
        Edit::Details if !extras.has_details() => return None,
        Edit::Cover if cover.is_none() => return None,
        _ => jf.find(name).ok().filter(|ids| !ids.is_empty())?,
    };
    let r: Result<(), String> = ids.iter().try_for_each(|id| match edit {
        Edit::Cover => jf.set_cover(id, cover.unwrap_or_default()),
        _ => jf.set_details(id, extras),
    });
    match r {
        Ok(()) => {
            crate::info!("[Jellyfin] {name}：只透過 Jellyfin 更新{}，沒有重傳", if edit == Edit::Cover { "封面" } else { "影片資訊" });
            Some(match edit {
                Edit::Cover => tr!("已換上 Jellyfin 的封面（不用重傳整支影片）", "Cover updated in Jellyfin (no need to re-send the whole video)").to_string(),
                _ => tr!("已更新 Jellyfin 上的影片資訊（不用重傳整支影片）", "Details updated in Jellyfin (no need to re-send the whole video)").to_string(),
            })
        }
        Err(e) => {
            crate::warn!("[Jellyfin] {name} 只透過 Jellyfin 更新失敗，改成整支重傳：{e}");
            None
        }
    }
}

/// 本機改過封面、影片資訊，只透過 Jellyfin 更新時，本機和媒體庫那一份的大小不一樣：（小寫檔名, 本機大小, 媒體庫大小）。
/// 存在設定資料夾的 library-links.json；沒有 init_links 時（測試）只記在記憶體
static LINKS: std::sync::Mutex<Option<(Option<std::path::PathBuf>, Vec<Link>)>> = std::sync::Mutex::new(None);
/// （小寫檔名, 本機大小, 媒體庫大小）
type Link = (String, u64, u64);

/// 讀出記下的對照（程式啟動時；已經讀過就不再讀）
pub fn init_links(file: std::path::PathBuf) {
    let mut g = LINKS.lock().unwrap();
    if g.as_ref().is_some_and(|(f, _)| f.is_some()) {
        return;
    }
    let mut list: Vec<(String, u64, u64)> = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    list.extend(g.take().map(|(_, l)| l).unwrap_or_default());
    *g = Some((Some(file), list));
}

fn with_links<T>(f: impl FnOnce(&mut Vec<(String, u64, u64)>) -> (T, bool)) -> T {
    let mut g = LINKS.lock().unwrap();
    let (file, list) = g.get_or_insert_with(|| (None, vec![]));
    let (out, changed) = f(list);
    if changed {
        if let Some(file) = file.as_ref() {
            if let Err(e) = std::fs::write(file, serde_json::to_string(list).unwrap_or_default()) {
                crate::warn!("[傳到資料夾] 記下媒體庫對照失敗：{e}");
            }
        }
    }
    out
}

/// 記下：本機這個檔名、這個大小的檔案，就是媒體庫裡那個大小的那一份（同一份之前的記錄換掉；最多記 2000 筆）
pub fn remember_link(name: &str, local: u64, lib: u64) {
    let name = name.to_lowercase();
    with_links(|list| {
        list.retain(|(n, _, b)| !(*n == name && *b == lib));
        if local != lib {
            list.push((name, local, lib));
        }
        let over = list.len().saturating_sub(2000);
        list.drain(..over);
        ((), true)
    })
}

/// 本機改名後，記下的對照也換成新名稱
pub fn rename_link(old: &str, new: &str) {
    let (old, new) = (old.to_lowercase(), new.to_lowercase());
    with_links(|list| {
        let mut changed = false;
        for (n, _, _) in list.iter_mut().filter(|(n, _, _)| *n == old) {
            *n = new.clone();
            changed = true;
        }
        ((), changed)
    })
}

/// 本機這個檔案（檔名、大小）和媒體庫裡這個大小的檔案是不是同一份：大小一樣，或記下過對照
pub fn same_copy(name: &str, local: u64, lib: u64) -> bool {
    local == lib || {
        let name = name.to_lowercase();
        with_links(|list| (list.iter().any(|(n, a, b)| *n == name && *a == local && *b == lib), false))
    }
}

/// 對照到媒體庫裡這個檔名、這個大小的本機大小（「還沒傳到媒體庫」篩選用）
pub fn linked_sizes(name: &str, lib: u64) -> Vec<u64> {
    let name = name.to_lowercase();
    with_links(|list| (list.iter().filter(|(n, _, b)| *n == name && *b == lib).map(|(_, a, _)| *a).collect(), false))
}

/// 把檔案（連同旁邊的字幕）傳到「複製」的資料夾（NAS、Jellyfin 媒體庫）：同名的覆蓋（同一支影片的新版本），
/// 有設定 Jellyfin 時請它重新讀取（新的檔案就請它掃描）。剪輯、加速版另存好、或使用者按「傳到」時用。
/// 回傳給使用者看的結果；沒設定「複製」資料夾時 None
pub fn send_file(jf: Option<&Jellyfin>, copy_to: &str, file: &std::path::Path, extras: &Extras) -> Option<Result<String, String>> {
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
        // 先看 NAS 的空間夠不夠（不夠時傳到一半才失敗，還留下半個暫存檔）
        let need: u64 = files.iter().filter_map(|f| std::fs::metadata(f).ok()).map(|m| m.len()).sum();
        if let Some(e) = space_problem(crate::recorder::disk_free_sync(copy_to), need) {
            crate::warn!("[傳到資料夾] {name} → {copy_to}：{e}");
            return Some(Err(trf!("沒有傳到「{folder}」：{e}", "Couldn't send it to “{folder}”: {e}")));
        }
        let copied = std::fs::create_dir_all(lib).and_then(|_| {
            for f in &files {
                crate::transfer::copy(f, &lib.join(library_name(f)))?;
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
    // 新的檔案等它掃到（最多 1 分鐘）；已經有的直接設定
    Some(match jf.refresh_items(&name, Some(&lib.join(library_name(file)))) {
        Ok(ids) => {
            let what = if ids.is_empty() { tr!("，已請 Jellyfin 掃描", ", Jellyfin asked to scan") } else { tr!("，已請 Jellyfin 重新讀取", ", Jellyfin asked to reload it") };
            let more = jf.after_upload(&name, extras, ids, Duration::from_secs(60), Some(&lib.join(library_name(file))));
            Ok(format!("{sent}{what}{more}"))
        }
        Err(e) => {
            crate::warn!("[Jellyfin] 通知 {name} 失敗：{e}");
            Err(format!("{sent}{}", trf!("，但沒有通知到 Jellyfin：{e}", ", but Jellyfin couldn't be notified: {e}")))
        }
    })
}

/// 空間不夠時的說明（留 200 MB 給 NAS 自己用；查不到剩多少時不擋）
pub fn space_problem(free: Option<u64>, need: u64) -> Option<String> {
    let free = free?;
    let fmt = crate::format::format_bytes;
    (free < need.saturating_add(200 << 20))
        .then(|| trf!("NAS 只剩 {}，這支要 {}。先到 NAS 清出空間再傳", "The NAS only has {} free and this needs {}. Free up some space on the NAS first", fmt(free), fmt(need)))
}

/// NAS 自己的資料夾（資源回收筒、縮圖快取…），不當成媒體庫的子資料夾
fn is_system_dir(name: &str) -> bool {
    name.starts_with(['#', '.', '@', '$']) || name.eq_ignore_ascii_case("System Volume Information")
}

/// 最多讀幾個資料夾（媒體庫很大時不會一直讀下去；資料夾連結不會進去）
const MAX_DIRS: usize = 20_000;

/// 這個資料夾底下一層的子資料夾名稱（依名稱排序；NAS 的系統資料夾不列），「傳到哪個資料夾」一層層點進去用；讀不到時是空的
pub fn subfolders(dir: &str) -> Vec<String> {
    let dir = dir.trim();
    if dir.is_empty() {
        return vec![];
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut out: Vec<String> = rd.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| !is_system_dir(n)).collect();
    out.sort_by_key(|n| n.to_lowercase());
    out
}

/// 子資料夾的每一層（「family/2024」或「family\2024」→ family、2024）
fn sub_parts(sub: &str) -> impl Iterator<Item = &str> {
    sub.split(['/', '\\']).map(str::trim).filter(|s| !s.is_empty())
}

/// 子資料夾（可以好幾層，用 / 或 \ 分開）能不能用：沒有不能用的符號、不是 . 或 ..、不是 NAS 的系統資料夾；空的 = 媒體庫最上層
pub fn valid_subfolder(sub: &str) -> bool {
    sub_parts(sub).all(|p| !p.contains([':', '*', '?', '"', '<', '>', '|']) && p != "." && p != ".." && !is_system_dir(p))
}

/// 子資料夾統一寫成「family/2024」（設定裡存這個）
pub fn normalize_subfolder(sub: &str) -> String {
    sub_parts(sub).collect::<Vec<_>>().join("/")
}

/// 傳到媒體庫的哪個資料夾：最上層，或底下的子資料夾（可以好幾層；不能用時放最上層）
pub fn library_dest(copy_to: &str, sub: &str) -> String {
    let copy_to = copy_to.trim();
    if copy_to.is_empty() || !valid_subfolder(sub) {
        return copy_to.to_string();
    }
    let mut p = std::path::PathBuf::from(copy_to);
    p.extend(sub_parts(sub));
    p.display().to_string()
}

/// 媒體庫的所有資料夾（最上層排第一，往下每一層都找；NAS 的系統資料夾和資料夾連結不進去），和它相對最上層的位置（「family/2024」；最上層是空的）
fn library_dirs(copy_to: &str) -> Vec<(std::path::PathBuf, String)> {
    let copy_to = copy_to.trim();
    if copy_to.is_empty() {
        return vec![];
    }
    let mut out = vec![(std::path::PathBuf::from(copy_to), String::new())];
    let mut i = 0;
    while i < out.len() && out.len() < MAX_DIRS {
        let (dir, rel) = out[i].clone();
        i += 1;
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        let mut subs: Vec<_> = rd.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| !is_system_dir(n)).collect();
        subs.sort_by_key(|n| n.to_lowercase());
        for n in subs {
            let r = if rel.is_empty() { n.clone() } else { format!("{rel}/{n}") };
            out.push((dir.join(&n), r));
        }
    }
    out
}

/// 檔案本來就放在媒體庫（最上層或底下任何一層，不含 NAS 的系統資料夾）裡：改名、刪除就是改到媒體庫那一份
pub fn in_library(copy_to: &str, file: &std::path::Path) -> bool {
    let copy_to = copy_to.trim();
    if copy_to.is_empty() {
        return false;
    }
    let root = std::path::Path::new(copy_to);
    for dir in file.ancestors().skip(1) {
        if same_dir(dir, root) {
            return true;
        }
        if dir.file_name().is_some_and(|n| is_system_dir(&n.to_string_lossy())) {
            return false;
        }
    }
    false
}

/// 在這些資料夾裡找同名的影片；size 有給時大小也要一樣，別的資料夾裡剛好同名的另一支不算
fn find_in_dirs(dirs: &[(std::path::PathBuf, String)], name: &str, size: Option<u64>) -> Vec<std::path::PathBuf> {
    dirs.iter().map(|(d, _)| d.join(name)).filter(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file() && size.is_none_or(|s| same_copy(name, s, m.len())))).collect()
}

/// 媒體庫裡（最上層和底下每一層）同名的影片；size 有給時大小也要一樣
pub fn find_in_library(copy_to: &str, name: &str, size: Option<u64>) -> Vec<std::path::PathBuf> {
    find_in_dirs(&library_dirs(copy_to), name, size)
}

/// 這些檔案已經在媒體庫的哪些位置（同名同大小；「family/2024」，最上層是空的）。整個媒體庫只讀一次
pub fn library_places(copy_to: &str, files: &[std::path::PathBuf]) -> Vec<String> {
    let dirs = library_dirs(copy_to);
    let mut out: Vec<String> = vec![];
    for f in files {
        let (Some(name), Ok(m)) = (f.file_name(), std::fs::metadata(f)) else { continue };
        for (dir, rel) in &dirs {
            if std::fs::metadata(dir.join(name)).is_ok_and(|x| x.is_file() && x.len() == m.len()) && !out.contains(rel) {
                out.push(rel.clone());
            }
        }
    }
    out
}

/// 媒體庫裡有哪些檔案：小寫的檔名 → (在哪個位置（「family/2024」，最上層是空的）, 大小)。清單上標「已在媒體庫」用，每個資料夾只讀一次
pub fn library_index(copy_to: &str) -> std::collections::HashMap<String, Vec<(String, u64)>> {
    let mut out: std::collections::HashMap<String, Vec<(String, u64)>> = std::collections::HashMap::new();
    for (dir, rel) in library_dirs(copy_to) {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let Ok(m) = e.metadata() else { continue };
            if m.is_file() {
                out.entry(e.file_name().to_string_lossy().to_lowercase()).or_default().push((rel.clone(), m.len()));
            }
        }
    }
    out
}

/// 媒體庫裡屬於這個檔案的那一份：同名（size 有給時大小也一樣）的影片，和同一個資料夾裡它的字幕（同名.srt、同名.zh-TW.srt…）。
/// 找最上層和底下每一層；檔案本來就在媒體庫裡時是同一份，回傳空的；file 不用還在（改名、刪除之後也查得到）
pub fn library_copies(copy_to: &str, file: &std::path::Path, size: Option<u64>) -> Vec<std::path::PathBuf> {
    if copy_to.trim().is_empty() || in_library(copy_to, file) {
        return vec![];
    }
    copies_in(&library_dirs(copy_to), file, size)
}

/// 好幾個檔案在媒體庫裡的那一份（一次刪很多支時用：整個媒體庫只讀一次）；每個檔案用它現在的大小比對
pub fn library_copies_of(copy_to: &str, files: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
    if copy_to.trim().is_empty() {
        return vec![];
    }
    let dirs = library_dirs(copy_to);
    let mut out: Vec<std::path::PathBuf> = files.iter().filter(|f| !in_library(copy_to, f)).flat_map(|f| copies_in(&dirs, f, std::fs::metadata(f).ok().map(|m| m.len()))).collect();
    out.sort();
    out.dedup();
    out
}

/// 在這些資料夾裡找這個檔案的那一份和它的字幕
fn copies_in(dirs: &[(std::path::PathBuf, String)], file: &std::path::Path, size: Option<u64>) -> Vec<std::path::PathBuf> {
    let (Some(name), Some(stem)) = (file.file_name(), file.file_stem()) else { return vec![] };
    let stem = stem.to_string_lossy().to_lowercase();
    let mut out = vec![];
    for video in find_in_dirs(dirs, &name.to_string_lossy(), size) {
        let Some(Ok(dir)) = video.parent().map(std::fs::read_dir) else { continue };
        out.extend(dir.flatten().map(|e| e.path()).filter(|p| p.is_file()).filter(|p| {
            let n = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            n.strip_prefix(&stem).is_some_and(|rest| rest.starts_with('.') && [".srt", ".vtt", ".ass"].iter().any(|x| rest.ends_with(x)))
        }));
        out.push(video);
    }
    out.sort();
    out
}

/// 改名後：媒體庫裡那一份（連同字幕）也換成新名稱；回傳改好的檔案（新名稱）。改名不會改內容：用新檔案的大小比對，別的資料夾裡剛好同名的不動
pub fn rename_copies(copy_to: &str, old: &std::path::Path, new: &std::path::Path) -> Result<Vec<std::path::PathBuf>, String> {
    let (Some(old_stem), Some(new_stem)) = (old.file_stem(), new.file_stem()) else { return Ok(vec![]) };
    let skip = old_stem.to_string_lossy().chars().count();
    let new_stem = new_stem.to_string_lossy();
    let size = std::fs::metadata(new).ok().map(|m| m.len());
    let mut done = vec![];
    for f in library_copies(copy_to, old, size) {
        let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let to = f.with_file_name(format!("{new_stem}{}", name.chars().skip(skip).collect::<String>()));
        if to.exists() {
            return Err(trf!("媒體庫裡已經有「{}」", "The library already has “{}”", to.file_name().unwrap_or_default().to_string_lossy()));
        }
        std::fs::rename(&f, &to).map_err(|e| e.to_string())?;
        crate::info!("[傳到資料夾] 跟著改名：{} → {}", f.display(), to.display());
        done.push(to);
    }
    if !done.is_empty() {
        rename_link(&old.file_name().unwrap_or_default().to_string_lossy(), &new.file_name().unwrap_or_default().to_string_lossy());
    }
    Ok(done)
}

/// 兩個資料夾是不是同一個（不分大小寫、/ 和 \ 都算、結尾的分隔符號不算）
fn same_dir(a: &std::path::Path, b: &std::path::Path) -> bool {
    let norm = |p: &std::path::Path| p.display().to_string().replace('\\', "/").trim_end_matches('/').to_lowercase();
    norm(a) == norm(b)
}

/// 在區域網路上找 Jellyfin 伺服器：廣播「who is JellyfinServer?」到 UDP 7359，等 wait 收回覆。
/// 回傳（伺服器名稱, 網址），同一台只列一次；伺服器關掉「自動探索」或防火牆擋住時找不到
pub fn discover(wait: Duration) -> Vec<(String, String)> {
    discover_at(std::net::SocketAddr::from(([255, 255, 255, 255], 7359)), wait)
}

fn discover_at(target: std::net::SocketAddr, wait: Duration) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = vec![];
    let Ok(sock) = std::net::UdpSocket::bind(("0.0.0.0", 0)) else { return found };
    let _ = sock.set_broadcast(true);
    if let Err(e) = sock.send_to(b"who is JellyfinServer?", target) {
        crate::info!("[Jellyfin] 找伺服器時廣播失敗：{e}");
        return found;
    }
    let until = std::time::Instant::now() + wait;
    let mut buf = [0u8; 2048];
    while let Some(left) = until.checked_duration_since(std::time::Instant::now()).filter(|d| !d.is_zero()) {
        let _ = sock.set_read_timeout(Some(left));
        let Ok((n, from)) = sock.recv_from(&mut buf) else { break };
        if let Some((name, url)) = parse_discovery(&String::from_utf8_lossy(&buf[..n]), from) {
            if !found.iter().any(|(_, u)| *u == url) {
                found.push((name, url));
            }
        }
    }
    crate::info!("[Jellyfin] 區域網路上找到 {} 台伺服器", found.len());
    found
}

/// 伺服器對「who is JellyfinServer?」的回覆：{"Address":"http://192.168.1.10:8096","Name":"NAS",…}；
/// 沒有網址時用回覆來源的 IP 加上預設的 8096
fn parse_discovery(json: &str, from: std::net::SocketAddr) -> Option<(String, String)> {
    let v: Value = serde_json::from_str(json).ok()?;
    v.get("Id")?;
    let name = v.get("Name").and_then(Value::as_str).unwrap_or("Jellyfin").to_string();
    let url = v.get("Address").and_then(Value::as_str).and_then(normalize_url).unwrap_or_else(|| format!("http://{}:8096", from.ip()));
    Some((name, url))
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
    agent_with(Duration::from_secs(15))
}

/// 下載整支影片用：每次讀等久一點（NAS 從硬碟休眠醒來要幾秒）
fn agent_long() -> ureq::Agent {
    agent_with(Duration::from_secs(60))
}

fn agent_with(read: Duration) -> ureq::Agent {
    let b = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(8)).timeout_read(read);
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
        assert_eq!(Jellyfin::from_ui(Some(&ui)), Some(Jellyfin { url: "http://nas:8096".into(), key: "abc".into(), root: String::new(), collections: true }));
        assert!(Jellyfin::from_ui(Some(&serde_json::json!({ URL_KEY: "nas:8096" }))).is_none());
    }

    /// 讀完請求的內容再回應：沒讀完就關掉連線時，Windows 會送 RST，用戶端可能收不到回應
    fn read_body(r: &mut impl std::io::Read, headers: &[String]) {
        let len = headers.iter().find_map(|h| h.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.trim().parse::<usize>().ok()));
        r.read_exact(&mut vec![0; len.unwrap_or(0)]).unwrap();
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
            read_body(&mut r, &lines);
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
                let mut headers = vec![];
                loop {
                    let mut line = String::new();
                    r.read_line(&mut line).unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                    headers.push(line.trim().to_string());
                }
                read_body(&mut r, &headers);
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
        assert!(req[1].starts_with("POST /Items/a1/Refresh?metadataRefreshMode=Default&imageRefreshMode=FullRefresh&replaceAllMetadata=false&replaceAllImages=true"), "{req:?}");
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
        assert!(sync_edited(None, &copy_to, &video, &Extras::default(), Edit::Video, None).is_none());
        assert!(sync_edited(None, "", &video, &Extras::default(), Edit::Video, None).is_none());
        // 有同名的一份：換成改好的
        std::fs::write(lib.join("a.mp4"), "old").unwrap();
        assert!(sync_edited(None, &copy_to, &video, &Extras::default(), Edit::Video, None).unwrap().is_ok());
        assert_eq!(std::fs::read_to_string(lib.join("a.mp4")).unwrap(), "new");
        // 直接改媒體庫裡的檔案（結尾有分隔符號、大小寫不同也算同一個資料夾）：不用複製，只通知
        let (url, h) = serve_seq(vec![("200 OK", r#"{"Items":[{"Id":"x","Path":"/m/a.mp4"}]}"#.to_string()), ("204 No Content", String::new())]);
        let jf = Jellyfin::new(&url, "k").unwrap();
        let msg = sync_edited(Some(&jf), &format!("{copy_to}/"), &lib.join("a.mp4"), &Extras::default(), Edit::Video, None).unwrap().unwrap();
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
        assert!(send_file(None, "", &video, &Extras::default()).is_none());
        // 資料夾還沒有：建立；字幕一起過去
        let copy_to = lib.display().to_string();
        let msg = send_file(None, &copy_to, &video, &Extras::default()).unwrap().unwrap();
        assert!(msg.contains("nas") && msg.contains("Rec_1_cut.mp4"), "{msg}");
        assert_eq!(std::fs::read_to_string(lib.join("Rec_1_cut.srt")).unwrap(), "subs");
        // 再傳一次新版本：覆蓋同名的，不會多出 _2
        std::fs::write(&video, "v3").unwrap();
        send_file(None, &copy_to, &video, &Extras::default()).unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(lib.join("Rec_1_cut.mp4")).unwrap(), "v3");
        assert_eq!(std::fs::read_dir(&lib).unwrap().count(), 2);
        // 有 Jellyfin：新檔案請它掃描，等它收到
        let found = r#"{"Items":[{"Id":"n1","Path":"/nas/Rec_1_cut.mp4"}]}"#.to_string();
        let (url, h) = serve_seq(vec![("200 OK", r#"{"Items":[]}"#.to_string()), ("204 No Content", String::new()), ("200 OK", found)]);
        let jf = Jellyfin::new(&url, "k").unwrap();
        let msg = send_file(Some(&jf), &copy_to, &video, &Extras::default()).unwrap().unwrap();
        assert!(msg.contains("✓"), "{msg}");
        let req = h.join().unwrap();
        assert!(req[1].starts_with("POST /Library/Refresh "));
        assert!(req[2].starts_with("GET /Items?"));
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

    #[test]
    fn extras_tags_links_and_chapter_task() {
        let meta = ";FFMETADATA1\ntitle=x\nkeywords=報價\\; 新人、報價\n[CHAPTER]\nTIMEBASE=1/1000\n";
        assert_eq!(Extras::from_ffmetadata(meta), Extras { tags: vec!["報價".into(), "新人".into()], chapters: true, title: "x".into(), ..Default::default() });
        let x = Extras::from_ffmetadata(";FFMETADATA1\nTITLE=報價單怎麼填\ncomment=第一行\\\n第二行\ngenre=教學; 業務\ndate=2026-10-19\n");
        assert_eq!((x.title.as_str(), x.overview.as_str(), x.genres.clone(), x.year), ("報價單怎麼填", "第一行\n第二行", vec!["教學".to_string(), "業務".into()], Some(2026)));
        assert_eq!(Extras::from_ffmetadata(";FFMETADATA1\n"), Extras::default());
        let jf = Jellyfin::new("nas:8096", "k").unwrap();
        assert_eq!(jf.link("abc", Some("s1")), "http://nas:8096/web/#/details?id=abc&serverId=s1");
        assert_eq!(jf.link("abc", None), "http://nas:8096/web/#/details?id=abc");
        let item = with_details(
            r#"{"Id":"abc","Name":"Rec","Overview":"舊","Genres":["教學"],"Tags":["old"]}"#,
            &Extras { tags: vec!["新".into()], title: "報價單".into(), year: Some(2026), ..Default::default() },
        )
        .unwrap();
        let v: Value = serde_json::from_str(&item).unwrap();
        assert_eq!(v["Tags"], serde_json::json!(["新"]));
        assert_eq!(v["Name"], "報價單");
        assert_eq!(v["ProductionYear"], 2026);
        // 沒填的照舊
        assert_eq!(v["Genres"], serde_json::json!(["教學"]));
        assert_eq!(v["Overview"], "舊");
        assert_eq!(task_id(r#"[{"Key":"Other","Id":"1"},{"Key":"RefreshChapterImages","Id":"42"}]"#, "RefreshChapterImages").as_deref(), Some("42"));
        assert!(chapter_images_enabled(r#"[{"LibraryOptions":{"EnableChapterImageExtraction":false}},{"LibraryOptions":{"EnableChapterImageExtraction":true}}]"#));
        assert!(!chapter_images_enabled(r#"[{"LibraryOptions":{}}]"#));
        // 標籤：取回整筆、換掉 Tags 再送回去
        let (url, h) = serve_seq(vec![("200 OK", r#"{"Id":"abc","Name":"Rec","Tags":[]}"#.to_string()), ("204 No Content", String::new())]);
        Jellyfin::new(&url, "k").unwrap().set_details("abc", &Extras { tags: vec!["新".into()], ..Default::default() }).unwrap();
        let req = h.join().unwrap();
        assert!(req[0].starts_with("GET /Items/abc "), "{req:?}");
        assert!(req[1].starts_with("POST /Items/abc "), "{req:?}");
    }

    #[test]
    fn chinese_subtitles_get_a_language_in_the_library() {
        let dir = tempfile::tempdir().unwrap();
        let zh = dir.path().join("Rec_1.srt");
        std::fs::write(&zh, "1\n00:00:00,000 --> 00:00:01,000\n大家好\n").unwrap();
        assert_eq!(library_name(&zh), "Rec_1.zh-TW.srt");
        let en = dir.path().join("Rec_2.srt");
        std::fs::write(&en, "1\n00:00:00,000 --> 00:00:01,000\nHello\n").unwrap();
        assert_eq!(library_name(&en), "Rec_2.srt");
        assert_eq!(library_name(&dir.path().join("Rec_1.en.srt")), "Rec_1.en.srt");
        assert_eq!(library_name(&dir.path().join("Rec_1.mp4")), "Rec_1.mp4");
    }

    #[test]
    fn library_copies_follow_renames_and_skip_the_same_folder() {
        let dir = tempfile::tempdir().unwrap();
        let (rec, lib) = (dir.path().join("rec"), dir.path().join("lib"));
        std::fs::create_dir_all(&rec).unwrap();
        std::fs::create_dir_all(&lib).unwrap();
        for f in ["會議.mp4", "會議.zh-TW.srt", "會議.en.srt", "會議_2x.mp4", "會議 2.mp4", "其他.mp4"] {
            std::fs::write(lib.join(f), "x").unwrap();
        }
        let copy_to = lib.display().to_string();
        let names = |v: Vec<std::path::PathBuf>| v.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect::<Vec<_>>();
        // 只有同名的影片和字幕，加速版、名字開頭一樣的別支不算
        assert_eq!(names(library_copies(&copy_to, &rec.join("會議.mp4"), None)), ["會議.en.srt", "會議.mp4", "會議.zh-TW.srt"]);
        assert_eq!(names(library_copies(&copy_to, &rec.join("會議_2x.mp4"), None)), ["會議_2x.mp4"]);
        // 本來就在媒體庫裡：同一份，不算
        assert!(library_copies(&copy_to, &lib.join("會議.mp4"), None).is_empty());
        assert!(in_library(&format!("{copy_to}/"), &lib.join("會議.mp4")));
        assert!(library_copies("", &rec.join("會議.mp4"), None).is_empty());

        assert_eq!(rename_copies(&copy_to, &rec.join("會議.mp4"), &rec.join("週會.mp4")).unwrap().len(), 3);
        assert_eq!(names(library_copies(&copy_to, &rec.join("週會.mp4"), None)), ["週會.en.srt", "週會.mp4", "週會.zh-TW.srt"]);
        assert!(library_copies(&copy_to, &rec.join("會議.mp4"), None).is_empty());
        // 新名稱媒體庫裡已經有：不蓋掉
        assert!(rename_copies(&copy_to, &rec.join("週會.mp4"), &rec.join("其他.mp4")).is_err());
        assert_eq!(rename_copies(&copy_to, &rec.join("沒傳過.mp4"), &rec.join("新.mp4")).unwrap().len(), 0);
    }

    #[test]
    fn library_subfolders_are_searched_all_the_way_down() {
        let dir = tempfile::tempdir().unwrap();
        let (rec, lib) = (dir.path().join("rec"), dir.path().join("video"));
        for d in ["baseball", "family/2024/活動", "#recycle", "@eaDir", "family/@eaDir"] {
            std::fs::create_dir_all(lib.join(d)).unwrap();
        }
        std::fs::create_dir_all(&rec).unwrap();
        let copy_to = lib.display().to_string();
        // 一層層點進去：每次只列底下一層，系統資料夾不列
        assert_eq!(subfolders(&copy_to), ["baseball", "family"]);
        assert_eq!(subfolders(&library_dest(&copy_to, "family")), ["2024"]);
        assert!(subfolders("").is_empty() && subfolders(&dir.path().join("沒有").display().to_string()).is_empty());
        // 子資料夾可以好幾層，/ 和 \ 都可以
        assert_eq!(library_dest(&copy_to, "family/2024"), lib.join("family").join("2024").display().to_string());
        assert_eq!(library_dest(&copy_to, "family\\2024"), library_dest(&copy_to, "family/2024"));
        assert_eq!(normalize_subfolder(" family\\2024/ "), "family/2024");
        assert_eq!(library_dest(&copy_to, ""), copy_to);
        assert_eq!(library_dest(&copy_to, "../x"), copy_to);
        assert_eq!(library_dest(&copy_to, "family/#recycle"), copy_to);
        assert!(valid_subfolder("新資料夾") && valid_subfolder("family/2024") && !valid_subfolder("a:b") && !valid_subfolder("#recycle"));

        // 本機的錄影 12 bytes；family/2024/活動 裡同名同大小（傳過去的）、baseball 裡同名但大小不同（別支）、系統資料夾裡的不算
        std::fs::write(rec.join("Rec.mp4"), "123456789012").unwrap();
        std::fs::write(lib.join("family/2024/活動/Rec.mp4"), "123456789012").unwrap();
        std::fs::write(lib.join("family/2024/活動/Rec.zh-TW.srt"), "x").unwrap();
        std::fs::write(lib.join("baseball/Rec.mp4"), "other").unwrap();
        std::fs::write(lib.join("#recycle/Rec.mp4"), "123456789012").unwrap();
        std::fs::write(lib.join("family/@eaDir/Rec.mp4"), "123456789012").unwrap();
        let names = |v: Vec<std::path::PathBuf>| v.iter().map(|p| p.strip_prefix(&lib).unwrap().display().to_string().replace('\\', "/")).collect::<Vec<_>>();
        assert_eq!(names(library_copies(&copy_to, &rec.join("Rec.mp4"), Some(12))), ["family/2024/活動/Rec.mp4", "family/2024/活動/Rec.zh-TW.srt"]);
        assert_eq!(names(find_in_library(&copy_to, "Rec.mp4", None)), ["baseball/Rec.mp4", "family/2024/活動/Rec.mp4"]);
        assert_eq!(library_places(&copy_to, &[rec.join("Rec.mp4")]), ["family/2024/活動"]);
        let idx = library_index(&copy_to);
        let mut got = idx["rec.mp4"].clone();
        got.sort();
        assert_eq!(got, [("baseball".to_string(), 5), ("family/2024/活動".to_string(), 12)]);

        // 媒體庫裡任何一層的檔案就是媒體庫那一份；系統資料夾裡的不算
        assert!(in_library(&copy_to, &lib.join("family/2024/活動/Rec.mp4")) && in_library(&copy_to, &lib.join("Rec.mp4")));
        assert!(!in_library(&copy_to, &lib.join("#recycle/Rec.mp4")) && !in_library(&copy_to, &lib.join("family/@eaDir/Rec.mp4")));
        assert!(!in_library(&copy_to, &rec.join("Rec.mp4")));

        // 改名只動同大小的那一份，baseball 裡的別支不動
        std::fs::rename(rec.join("Rec.mp4"), rec.join("練琴.mp4")).unwrap();
        assert_eq!(rename_copies(&copy_to, &rec.join("Rec.mp4"), &rec.join("練琴.mp4")).unwrap().len(), 2);
        assert!(lib.join("family/2024/活動/練琴.mp4").is_file() && lib.join("family/2024/活動/練琴.zh-TW.srt").is_file() && lib.join("baseball/Rec.mp4").is_file());

        // 本機改了封面（大小不同）：只有一份同名的才換掉
        std::fs::write(rec.join("練琴.mp4"), "new content, longer").unwrap();
        assert!(sync_edited(None, &copy_to, &rec.join("練琴.mp4"), &Extras::default(), Edit::Video, None).unwrap().is_ok());
        assert_eq!(std::fs::read_to_string(lib.join("family/2024/活動/練琴.mp4")).unwrap(), "new content, longer");
        // 兩個資料夾都有同名的：不知道是哪一份，不動
        std::fs::write(rec.join("Rec.mp4"), "zzz").unwrap();
        std::fs::write(lib.join("Rec.mp4"), "root").unwrap();
        assert!(sync_edited(None, &copy_to, &rec.join("Rec.mp4"), &Extras::default(), Edit::Video, None).is_none());
        assert_eq!(std::fs::read_to_string(lib.join("baseball/Rec.mp4")).unwrap(), "other");
    }

    #[test]
    fn server_root_is_found_from_recent_items_and_folders_map_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("Y");
        std::fs::create_dir_all(lib.join("other")).unwrap();
        std::fs::write(lib.join("other/Rec_1.mp4"), "x").unwrap();
        let root = lib.display().to_string();
        // 別的媒體庫的影片對不上，略過；other/Rec_1.mp4 對得上 → 前面那段就是
        let items = r#"{"Items":[{"Path":"/volume1/movies/Film.mkv"},{"Path":"/volume1/video/other/Rec_1.mp4"}]}"#;
        assert_eq!(server_root_from(items, &root).as_deref(), Some("/volume1/video"));
        assert_eq!(server_root_from(r#"{"Items":[{"Path":"/x/none.mp4"}]}"#, &root), None);
        // Windows 伺服器
        assert_eq!(server_root_from(r#"{"Items":[{"Path":"D:\\Media\\other\\Rec_1.mp4"}]}"#, &root).as_deref(), Some("D:\\Media"));

        assert_eq!(server_path("/volume1/video", "Y:\\", std::path::Path::new("Y:\\family\\2024")).as_deref(), Some("/volume1/video/family/2024"));
        assert_eq!(server_path("/volume1/video/", "y:\\", std::path::Path::new("Y:\\")).as_deref(), Some("/volume1/video"));
        assert_eq!(server_path("D:\\Media", "\\\\nas\\video", std::path::Path::new("\\\\NAS\\video\\other")).as_deref(), Some("D:\\Media\\other"));
        assert_eq!(server_path("/", "Y:", std::path::Path::new("Y:\\a")).as_deref(), Some("/a"));
        // 不在媒體庫裡、名稱只是開頭一樣
        assert_eq!(server_path("/v", "Y:\\video", std::path::Path::new("Y:\\videos\\a")), None);
        assert_eq!(server_path("/v", "Y:\\video", std::path::Path::new("C:\\rec")), None);
    }

    #[test]
    fn scan_asks_for_just_the_folder_when_the_server_path_is_known() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib");
        std::fs::create_dir_all(lib.join("family")).unwrap();
        std::fs::write(lib.join("family/old.mp4"), "x").unwrap();
        let items = r#"{"Items":[{"Path":"/srv/video/family/old.mp4"}]}"#.to_string();
        let (url, h) = serve_seq(vec![("200 OK", items), ("204 No Content", String::new())]);
        let jf = Jellyfin::new(&url, "k").unwrap().with_root(&lib.display().to_string());
        assert!(jf.scan(&lib.join("family/new.mp4")).unwrap());
        let got = h.join().unwrap();
        assert!(got[0].starts_with("GET /Items?"), "{got:?}");
        assert!(got[1].starts_with("POST /Library/Media/Updated"), "{got:?}");
        // 對不上（媒體庫還沒有影片）：掃描所有媒體庫
        let (url, h) = serve_seq(vec![("200 OK", r#"{"Items":[]}"#.to_string()), ("204 No Content", String::new())]);
        let jf = Jellyfin::new(&url, "k").unwrap().with_root(&dir.path().join("empty").display().to_string());
        assert!(!jf.scan(&dir.path().join("empty/a.mp4")).unwrap());
        assert!(h.join().unwrap()[1].starts_with("POST /Library/Refresh"));
    }

    #[test]
    fn library_type_is_checked() {
        let vf = r#"[{"Name":"錄影","CollectionType":"homevideos","Locations":["/volume1/video"]},
                     {"Name":"電影","CollectionType":"movies","Locations":["/volume1/movies"]},
                     {"Name":"混合","Locations":["/volume1/mix/"]}]"#;
        assert!(library_check(vf, Some("/volume1/video")).unwrap().ok);
        let n = library_check(vf, Some("/volume1/movies/sub")).unwrap().note;
        assert!(n.contains("電影") || n.contains("Movies"), "{n}");
        assert!(n.contains("家庭影片") || n.contains("Home Videos"), "{n}");
        let n = library_check(vf, Some("/volume1/mix")).unwrap().note;
        assert!(n.contains("混合") || n.contains("Mixed"), "{n}");
        assert_eq!(library_check(vf, Some("/other")), None);
        assert_eq!(library_check(vf, None), None);
    }

    #[test]
    fn servers_are_discovered_over_udp() {
        let srv = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let at = srv.local_addr().unwrap();
        let h = std::thread::spawn(move || {
            let mut buf = [0u8; 64];
            let (n, from) = srv.recv_from(&mut buf).unwrap();
            assert_eq!(&buf[..n], b"who is JellyfinServer?");
            let reply = r#"{"Address":"http://192.168.1.10:8096","Id":"abc","Name":"家裡的NAS","EndpointAddress":null}"#;
            srv.send_to(reply.as_bytes(), from).unwrap();
            srv.send_to(reply.as_bytes(), from).unwrap();
            srv.send_to(b"not json", from).unwrap();
        });
        let found = discover_at(at, Duration::from_millis(500));
        h.join().unwrap();
        assert_eq!(found, vec![("家裡的NAS".to_string(), "http://192.168.1.10:8096".to_string())]);
        let from = std::net::SocketAddr::from(([10, 0, 0, 5], 7359));
        assert_eq!(parse_discovery(r#"{"Id":"x","Name":"N"}"#, from), Some(("N".into(), "http://10.0.0.5:8096".into())));
        assert_eq!(parse_discovery(r#"{"Name":"N"}"#, from), None);
    }

    #[test]
    fn cover_changes_go_through_jellyfin_without_re_sending() {
        let dir = tempfile::tempdir().unwrap();
        let (local, lib) = (dir.path().join("local"), dir.path().join("lib"));
        std::fs::create_dir_all(&local).unwrap();
        std::fs::create_dir_all(lib.join("family")).unwrap();
        let video = local.join("j3封面.mp4");
        std::fs::write(&video, "new cover!").unwrap();
        std::fs::write(lib.join("family/j3封面.mp4"), "old").unwrap();
        let copy_to = lib.display().to_string();
        let items = r#"{"Items":[{"Id":"x1","Path":"/m/family/j3封面.mp4"}]}"#.to_string();
        let (url, h) = serve_seq(vec![("200 OK", items), ("204 No Content", String::new())]);
        let jf = Jellyfin::new(&url, "k").unwrap();
        let msg = sync_edited(Some(&jf), &copy_to, &video, &Extras::default(), Edit::Cover, Some(b"jpeg")).unwrap().unwrap();
        assert!(msg.contains("Jellyfin"), "{msg}");
        let got = h.join().unwrap();
        assert!(got[1].starts_with("POST /Items/x1/Images/Primary"), "{got:?}");
        // 媒體庫那一份沒動，但大小不同也認得是同一份
        assert_eq!(std::fs::read_to_string(lib.join("family/j3封面.mp4")).unwrap(), "old");
        assert!(same_copy("J3封面.MP4", 10, 3));
        assert_eq!(library_copies(&copy_to, &video, Some(10)), vec![lib.join("family/j3封面.mp4")]);
        assert_eq!(linked_sizes("j3封面.mp4", 3), vec![10]);
        // 改名跟著走
        let renamed = local.join("j3新名.mp4");
        std::fs::rename(&video, &renamed).unwrap();
        assert_eq!(rename_copies(&copy_to, &video, &renamed).unwrap(), vec![lib.join("family/j3新名.mp4")]);
        assert!(same_copy("j3新名.mp4", 10, 3));
        // 換畫面（旋轉）：整支重傳；Jellyfin 還沒有這支時封面也整支重傳
        let (url, h) = serve_seq(vec![("200 OK", r#"{"Items":[]}"#.to_string()), ("200 OK", r#"{"Items":[]}"#.to_string()), ("204 No Content", String::new())]);
        let jf = Jellyfin::new(&url, "k").unwrap();
        sync_edited(Some(&jf), &copy_to, &renamed, &Extras::default(), Edit::Cover, Some(b"jpeg")).unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(lib.join("family/j3新名.mp4")).unwrap(), "new cover!");
        drop(h);
    }

    #[test]
    fn recording_time_comes_from_the_name_or_the_file() {
        let t = |s: &str| name_time(s).map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string());
        assert_eq!(t("Rec_2026-10-20_07-30-05").as_deref(), Some("2026-10-20 07:30:05"));
        assert_eq!(t("PXL_20261012_145733464").as_deref(), Some("2026-10-12 14:57:33"));
        assert_eq!(t("VID_20240501_103000_cut").as_deref(), Some("2024-05-01 10:30:00"));
        assert_eq!(t("Velfie_2_cut"), None);
        assert_eq!(t("Rec_2026-13-40_07-30-05"), None);
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("Velfie_2.mp4");
        std::fs::write(&f, "x").unwrap();
        crate::paths::set_mtime(&f, std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000));
        assert_eq!(recorded_at(&f).as_deref(), Some("2023-11-14T22:13:20.0000000Z"));
        // 寫進影片資料：首映日期、加入日期；沒填年份時用錄影的年份
        let x = Extras { recorded: recorded_at(&f), ..Default::default() };
        assert!(x.has_details());
        let item: Value = serde_json::from_str(&with_details(r#"{"Name":"a","ProductionYear":null}"#, &x).unwrap()).unwrap();
        assert_eq!(item["PremiereDate"], "2023-11-14T22:13:20.0000000Z");
        assert_eq!(item["DateCreated"], "2023-11-14T22:13:20.0000000Z");
        assert_eq!(item["ProductionYear"], 2023);
        let item: Value = serde_json::from_str(&with_details(r#"{"ProductionYear":2020}"#, &x).unwrap()).unwrap();
        assert_eq!(item["ProductionYear"], 2020);
    }

    #[test]
    fn collections_follow_the_folder() {
        let p = std::path::Path::new;
        assert_eq!(collection_name("Y:\\", p("Y:\\family\\2024\\a.mp4")).as_deref(), Some("family 2024"));
        assert_eq!(collection_name("Y:\\", p("Y:\\a.mp4")), None);
        assert_eq!(collection_name("\\\\nas\\video", p("\\\\NAS\\video\\baseball\\a.mp4")).as_deref(), Some("baseball"));
        assert_eq!(collection_name("Y:\\video", p("Y:\\videos\\x\\a.mp4")), None);
        assert_eq!(collection_name("", p("Y:\\x\\a.mp4")), None);
        let items = r#"{"Items":[{"Id":"c1","Name":"Family 2024"},{"Id":"c2","Name":"family"}]}"#;
        assert_eq!(collection_id(items, "family 2024").as_deref(), Some("c1"));
        assert_eq!(collection_id(items, "baseball"), None);
    }

    #[test]
    fn backdrops_are_filled_only_when_missing() {
        assert_eq!(missing_images(r#"{"ImageTags":{"Primary":"p"},"BackdropImageTags":[]}"#), ["Backdrop", "Thumb"]);
        assert_eq!(missing_images(r#"{"ImageTags":{"Primary":"p","Thumb":"t"},"BackdropImageTags":["b"]}"#), Vec::<&str>::new());
        // 還沒有封面：沒東西可以補
        assert_eq!(missing_images(r#"{"ImageTags":{},"BackdropImageTags":[]}"#), Vec::<&str>::new());
    }

    #[test]
    fn library_options_are_checked_and_fixed() {
        let vf = r#"[{"Name":"錄影","CollectionType":"homevideos","ItemId":"L1","Locations":["/v"],
                      "LibraryOptions":{"EnableRealtimeMonitor":false,"EnableChapterImageExtraction":true,"EnableTrickplayImageExtraction":false,"Other":1}}]"#;
        let c = library_check(vf, Some("/v")).unwrap();
        assert!(!c.ok);
        assert!(c.note.contains("即時監控") || c.note.contains("real-time"), "{}", c.note);
        assert!(!(c.note.contains("擷取章節圖片") || c.note.contains("chapter images")), "{}", c.note);
        let fixed = with_good_options(&serde_json::json!({ "EnableRealtimeMonitor": false, "Other": 1 }));
        assert_eq!(fixed["EnableRealtimeMonitor"], true);
        assert_eq!(fixed["ExtractTrickplayImagesDuringLibraryScan"], true);
        assert_eq!(fixed["Other"], 1);
        assert!(library_option_on(vf, "EnableChapterImageExtraction") && !library_option_on(vf, "EnableTrickplayImageExtraction"));

        // 一鍵修好：已經是家庭影片 → 打開選項
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("x")).unwrap();
        std::fs::write(dir.path().join("x/a.mp4"), "x").unwrap();
        let items = r#"{"Items":[{"Path":"/v/x/a.mp4"}]}"#.to_string();
        let (url, h) = serve_seq(vec![("200 OK", items.clone()), ("200 OK", vf.to_string()), ("204 No Content", String::new())]);
        let jf = Jellyfin::new(&url, "k").unwrap().with_root(&dir.path().display().to_string());
        let msg = jf.fix_library().unwrap();
        assert!(msg.contains("錄影"), "{msg}");
        assert!(h.join().unwrap()[2].starts_with("POST /Library/VirtualFolders/LibraryOptions"));
        // 是電影類型 → 新建家庭影片媒體庫（名稱撞到時加 (2)）
        let movies = r#"[{"Name":"錄影","CollectionType":"movies","ItemId":"M","Locations":["/v"]}]"#.to_string();
        let (url, h) = serve_seq(vec![("200 OK", items), ("200 OK", movies), ("204 No Content", String::new())]);
        let jf = Jellyfin::new(&url, "k").unwrap().with_root(&dir.path().display().to_string());
        let msg = jf.fix_library().unwrap();
        assert!(msg.contains("錄影 (2)") || msg.contains("Recordings"), "{msg}");
        let got = h.join().unwrap();
        assert!(got[2].starts_with("POST /Library/VirtualFolders?") && got[2].contains("collectionType=homevideos"), "{got:?}");
    }

    #[test]
    fn watched_and_favorites_are_read_per_user() {
        let users = r#"[{"Id":"u1","Name":"媽媽"},{"Id":"u2","Name":"小明"}]"#.to_string();
        let played1 = r#"{"Items":[{"Path":"/v/family/Rec_1.mp4","UserData":{"LastPlayedDate":"2026-10-18T12:00:00Z","Played":true}}]}"#.to_string();
        let played2 = r#"{"Items":[{"Path":"D:\\v\\REC_1.mp4","UserData":{"Played":true}},{"Path":"/v/b.mp4"}]}"#.to_string();
        let favs = r#"{"Items":[{"Path":"/v/b.mp4"}]}"#.to_string();
        let (url, h) = serve_seq(vec![("200 OK", users), ("200 OK", played1), ("200 OK", played2), ("200 OK", favs)]);
        let st = Jellyfin::new(&url, "k").unwrap().watch_state(Some("u2")).unwrap();
        h.join().unwrap();
        assert_eq!(st.watched["rec_1.mp4"], vec![("媽媽".to_string(), "2026-10-18".to_string()), ("小明".to_string(), String::new())]);
        assert_eq!(st.watched["b.mp4"].len(), 1);
        assert!(st.favorites.contains("b.mp4") && st.fav_user.as_deref() == Some("u2"));
        // 和上次比：第一次全部是新加的；本機改了也記下
        let now: std::collections::HashSet<String> = ["fav_a.mp4".to_string(), "fav_b.mp4".to_string()].into();
        let (added, _) = favorites_changed(&now);
        assert!(added.contains(&"fav_a.mp4".to_string()));
        favorite_set("FAV_C.mp4", true);
        let (added, removed) = favorites_changed(&now);
        assert!(added.is_empty() && removed == ["fav_c.mp4"], "{added:?} {removed:?}");
    }

    #[test]
    fn people_and_adults_only_go_to_jellyfin() {
        let x = Extras::from_ffmetadata(";FFMETADATA1\nkeywords=家庭; @爸爸; @小明; 18+\n");
        assert_eq!(x.people, ["爸爸", "小明"]);
        assert!(x.adult && x.tags == ["家庭", "18+"], "{:?}", x.tags);
        let item: Value = serde_json::from_str(&with_details(r#"{"Name":"a"}"#, &x).unwrap()).unwrap();
        assert_eq!(item["People"][1]["Name"], "小明");
        assert_eq!(item["People"][0]["Type"], "Actor");
        assert_eq!(item["OfficialRating"], "R");
        // 拿掉「只給大人看」：清掉 R；別的分級不動
        let kid = Extras { title: "t".into(), ..Default::default() };
        let item: Value = serde_json::from_str(&with_details(r#"{"OfficialRating":"R"}"#, &kid).unwrap()).unwrap();
        assert_eq!(item["OfficialRating"], "");
        let item: Value = serde_json::from_str(&with_details(r#"{"OfficialRating":"PG"}"#, &kid).unwrap()).unwrap();
        assert_eq!(item["OfficialRating"], "PG");
        // 播放前檢查格式
        assert_eq!(crate::actions::tv_unfriendly(Some("hevc")).as_deref(), Some("HEVC"));
        assert_eq!(crate::actions::tv_unfriendly(Some("av1")).as_deref(), Some("AV1"));
        assert_eq!(crate::actions::tv_unfriendly(Some("h264")), None);
        assert_eq!(crate::actions::tv_unfriendly(None), None);
    }

    #[test]
    fn jellyfin_items_are_parsed_for_browsing() {
        let json = r#"{"TotalRecordCount":7,"Items":[
            {"Id":"f1","Name":"family","Type":"Folder","IsFolder":true,"ChildCount":12,"ImageTags":{}},
            {"Id":"v1","Name":"練琴","Type":"Video","Path":"/v/family/練琴.mp4","RunTimeTicks":1234000000,"PremiereDate":"2026-10-12T06:57:33Z",
             "MediaStreams":[{"Type":"Video","Width":1920,"Height":1080,"RealFrameRate":29.97},{"Type":"Audio"}],"UserData":{"Played":true},"ImageTags":{"Primary":"x"}},
            {"Id":"p1","Name":"照片","Type":"Photo"}]}"#;
        let (items, total) = parse_items(json);
        assert_eq!(total, 7);
        assert_eq!(items.len(), 2);
        assert_eq!((items[0].kind, items[0].children, items[0].has_image), (JfKind::Folder, 12, false));
        let v = &items[1];
        assert_eq!((v.kind, v.file_name.as_str(), v.width, v.height, v.has_audio, v.played, v.date.as_str()), (JfKind::Video, "練琴.mp4", 1920, 1080, true, true, "2026-10-12"));
        assert!((v.duration - 123.4).abs() < 1e-6 && (v.fps - 29.97).abs() < 1e-6);
        let jf = Jellyfin::new("nas:8096", "secret").unwrap();
        let url = jf.stream_url("v1");
        assert_eq!(url, "http://nas:8096/Videos/v1/stream?static=true&api_key=secret");
        assert!(!redact(&url).contains("secret") && redact("x?api_key=s&y=1").ends_with("&y=1"));
        assert_eq!(Jellyfin::item_of_stream(&url).as_deref(), Some("v1"));
        assert_eq!(Jellyfin::item_of_stream("D:\\a.mp4"), None);
    }

    #[test]
    fn deleting_reports_missing_permission() {
        let (url, h) = serve_seq(vec![("204 No Content", String::new()), ("403 Forbidden", String::new())]);
        let jf = Jellyfin::new(&url, "k").unwrap();
        jf.delete_item("v1").unwrap();
        let e = jf.delete_item("v2").unwrap_err();
        assert!(e.contains("允許刪除") || e.contains("Allow media deletion"), "{e}");
        let got = h.join().unwrap();
        assert!(got[0].starts_with("DELETE /Items/v1"), "{got:?}");
    }

    #[test]
    fn items_carry_resume_subtitles_and_chapters() {
        let json = r#"[{"Id":"v1","Name":"家庭","Type":"Video","RunTimeTicks":600000000,
            "UserData":{"PlaybackPositionTicks":125000000,"Played":false},
            "MediaStreams":[{"Type":"Video","Width":1920,"Height":1080},
                {"Type":"Subtitle","Index":2,"DisplayTitle":"Chinese","IsTextSubtitleStream":true,"IsExternal":true},
                {"Type":"Subtitle","Index":3,"DisplayTitle":"PGS","IsTextSubtitleStream":false}],
            "Chapters":[{"StartPositionTicks":0,"Name":"開頭"},{"StartPositionTicks":300000000,"Name":"中間"}]}]"#;
        let (items, total) = parse_items(json);
        assert_eq!(total, 1);
        let v = &items[0];
        assert_eq!(v.resume, 12.5);
        assert_eq!(v.subs, vec![JfSub { index: 2, title: "Chinese".into() }]);
        assert_eq!(v.chapters, vec![(0.0, "開頭".to_string()), (30.0, "中間".to_string())]);
    }

    #[test]
    fn task_progress_only_while_running() {
        let t = r#"[{"Key":"RefreshTrickplayImages","State":"Running","CurrentProgressPercentage":42.5},{"Key":"Other","State":"Idle"}]"#;
        assert_eq!(task_progress(t, "RefreshTrickplayImages"), Some(42.5));
        assert_eq!(task_progress(t, "Other"), None);
        assert_eq!(task_progress("[]", "Other"), None);
    }

    #[test]
    fn not_enough_space_on_the_nas() {
        assert!(space_problem(Some(100 << 20), 50 << 20).is_some());
        assert!(space_problem(Some(10 << 30), 1 << 30).is_none());
        assert!(space_problem(None, 1 << 40).is_none());
    }

    #[test]
    fn position_falls_back_to_older_servers() {
        // 10.10 的網址沒有（舊版）：改用 10.9 的
        let (url, h) = serve_seq(vec![("404 Not Found", String::new()), ("204 No Content", String::new())]);
        let jf = Jellyfin::new(&url, "k").unwrap();
        jf.save_position("u1", "v1", 12.5, false).unwrap();
        let got = h.join().unwrap();
        assert!(got[0].starts_with("POST /UserItems/v1/UserData?userId=u1"), "{got:?}");
        assert!(got[1].starts_with("POST /Users/u1/Items/v1/UserData"), "{got:?}");
    }

    #[test]
    fn filtered_browse_lists_videos_below_the_folder() {
        let (url, h) = serve_seq(vec![("200 OK", r#"{"Items":[],"TotalRecordCount":0}"#.to_string())]);
        let jf = Jellyfin::new(&url, "k").unwrap();
        jf.items("u", Some("p1"), "", 0, 60, JfSort::Name, JfFilter::Unplayed, None).unwrap();
        let got = h.join().unwrap();
        assert!(got[0].contains("filters=IsUnplayed") && got[0].contains("recursive=true") && got[0].contains("parentId=p1") && got[0].contains("sortBy=SortName"), "{got:?}");
    }

    #[test]
    fn facet_browse_searches_the_whole_server() {
        let (url, h) = serve_seq(vec![("200 OK", r#"{"Items":[],"TotalRecordCount":0}"#.to_string())]);
        let jf = Jellyfin::new(&url, "k").unwrap();
        jf.items("u", Some("p1"), "", 0, 60, JfSort::Name, JfFilter::All, Some(&JfFacet::Person("pp".into(), "媽媽".into()))).unwrap();
        let got = h.join().unwrap();
        assert!(got[0].contains("personIds=pp") && got[0].contains("recursive=true") && !got[0].contains("parentId"), "{got:?}");
    }

    #[test]
    fn trickplay_picks_a_size_and_finds_the_tile() {
        let v: Value = serde_json::from_str(r#"{"ms1":{"320":{"Width":320,"Height":180,"TileWidth":10,"TileHeight":10,"ThumbnailCount":250,"Interval":10000},"120":{"Width":120,"Height":68,"TileWidth":10,"TileHeight":10,"ThumbnailCount":250,"Interval":10000}}}"#).unwrap();
        let tp = trickplay_of(&v).unwrap();
        assert_eq!((tp.source.as_str(), tp.width, tp.interval), ("ms1", 320, 10.0));
        // 第 123 張（1230 秒）：第 1 張大圖的第 23 格（第 2 列第 3 個）
        let (index, uv) = tp.at(1235.0);
        assert_eq!(index, 1);
        assert!((uv[0] - 0.3).abs() < 1e-6 && (uv[1] - 0.2).abs() < 1e-6 && (uv[2] - 0.4).abs() < 1e-6);
        // 超過最後一張停在最後一張
        assert_eq!(tp.at(1e9).0, 2);
        assert!(trickplay_of(&serde_json::json!({})).is_none());
    }

    #[test]
    fn facets_persons_and_info() {
        let (tags, years) = facets_of(r#"{"Tags":["旅行","Birthday"],"Years":[2023,2025,2024]}"#);
        assert_eq!(tags, vec!["Birthday", "旅行"]);
        assert_eq!(years, vec![2025, 2024, 2023]);
        assert_eq!(persons_of(r#"{"Items":[{"Id":"p1","Name":"媽媽"},{"Id":"p2","Name":" "}]}"#), vec![("p1".to_string(), "媽媽".to_string())]);
        let item = r#"{"Id":"x","Name":"生日","Overview":"蛋糕","Tags":["家"],"ProductionYear":2024,"People":[{"Name":"小明","Type":"Actor"},{"Name":"爸爸","Type":"Director"}]}"#;
        let info = info_of(item).unwrap();
        assert_eq!(info, JfInfo { title: "生日".into(), overview: "蛋糕".into(), people: vec!["小明".into()], tags: vec!["家".into()], year: Some(2024) });
        let edited = JfInfo { title: "生日派對".into(), overview: String::new(), people: vec!["阿嬤".into()], tags: vec![], year: None };
        let v: Value = serde_json::from_str(&with_info(item, &edited).unwrap()).unwrap();
        assert_eq!(v["Name"], "生日派對");
        assert_eq!(v["Overview"], "");
        assert_eq!(v["Tags"], serde_json::json!([]));
        assert!(v["ProductionYear"].is_null());
        // 導演留著，演員換成新的
        assert_eq!(v["People"], serde_json::json!([{"Name":"爸爸","Type":"Director"},{"Name":"阿嬤","Type":"Actor"}]));
    }
}
