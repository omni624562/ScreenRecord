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

/// 「沒有人物／標籤」最近一次的結果（查詢條件, 什麼時候, 全部符合的）：換頁用
type LackingCache = Option<(String, std::time::Instant, std::sync::Arc<Vec<Value>>)>;
static LACKING: std::sync::Mutex<LackingCache> = std::sync::Mutex::new(None);

/// 影片的人物、標籤改了：「沒有人物／標籤」要重查
pub fn forget_lacking() {
    *LACKING.lock().unwrap_or_else(|e| e.into_inner()) = None;
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
        if let Some((found, at)) = SERVER_ROOTS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&key).cloned()) {
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
        SERVER_ROOTS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Default::default).insert(key, (found.clone(), std::time::Instant::now()));
        found
    }

    /// 請 Jellyfin 掃描剛放進去的檔案：知道伺服器上的路徑時，直接請它重新整理那個資料夾（Jellyfin 上還沒有這個資料夾時找最近的上層，
    /// 最上層就是那個媒體庫；和控制台的「掃描媒體庫」同一個做法，馬上開始）。做不到時改成通知它資料夾裡有新檔案
    /// （它會先等「媒體庫監控延遲」，預設 60 秒，才去看）。不知道伺服器上的路徑時掃描所有媒體庫。回傳是不是只掃描了那個資料夾或媒體庫
    pub fn scan(&self, local: &std::path::Path) -> Result<bool, String> {
        let dir = local.parent().unwrap_or(local);
        let Some(remote) = self.remote_dir(dir) else {
            return self.refresh().map(|_| false);
        };
        if let Some((id, path)) = self.folder_for(&remote) {
            let url = format!("{}/Items/{id}/Refresh?Recursive=true&MetadataRefreshMode=Default&ImageRefreshMode=Default&ReplaceAllMetadata=false&ReplaceAllImages=false", self.url);
            match self.call(agent().post(&url)) {
                Ok(_) => {
                    crate::info!("[Jellyfin] 請它掃描 {path}（{remote} 新的檔案）");
                    return Ok(true);
                }
                Err(e) => crate::info!("[Jellyfin] 重新整理資料夾 {path} 失敗，改成通知新檔案：{e}"),
            }
        } else if let Some(locs) = self.outside(&remote) {
            crate::warn!("[Jellyfin] {remote} 不在任何媒體庫裡，Jellyfin 不會掃到（媒體庫的資料夾：{}）", locs.join("、"));
        }
        let body = serde_json::json!({ "Updates": [{ "Path": remote, "UpdateType": "Created" }] }).to_string();
        match self.call_body(agent().post(&format!("{}/Library/Media/Updated", self.url)), &body) {
            Ok(_) => {
                crate::info!("[Jellyfin] 通知它 {remote} 有新檔案");
                Ok(true)
            }
            // 舊版或不支援：掃描所有媒體庫
            Err(_) => self.refresh().map(|_| false),
        }
    }

    /// 這台電腦媒體庫裡的資料夾在伺服器上的路徑（看不出來時 None）
    fn remote_dir(&self, dir: &std::path::Path) -> Option<String> {
        self.server_root().and_then(|r| server_path(&r, &self.root, dir))
    }

    /// 伺服器上的資料夾 remote 不在任何媒體庫裡時，所有媒體庫的資料夾；在媒體庫裡或查不到時 None
    fn outside(&self, remote: &str) -> Option<Vec<String>> {
        let libs = self.call(agent().get(&format!("{}/Library/VirtualFolders", self.url))).ok()?.into_string().ok()?;
        outside_libraries(&libs, remote)
    }

    /// 媒體庫資料夾（例如 \\HomeStorage\video）底下 Jellyfin 會掃描的子資料夾（「family」「porn/cut」；整個資料夾都掃時是 [""]），
    /// 「傳到哪個資料夾」標出看得到的、擋下看不到的。看不出伺服器上的路徑、或讀不到媒體庫清單時 None（不擋）
    pub fn library_subs(&self) -> Option<Vec<String>> {
        let root = self.server_root()?;
        let libs = self.call(agent().get(&format!("{}/Library/VirtualFolders", self.url))).ok()?.into_string().ok()?;
        subs_in_libraries(&libs, &root)
    }

    /// Jellyfin 上最接近伺服器路徑 remote 的資料夾或媒體庫（它本身或最近的上層）：（編號, 伺服器上的路徑）
    fn folder_for(&self, remote: &str) -> Option<(String, String)> {
        let get = |url: String| self.call(agent().get(&url)).ok().and_then(|r| r.into_string().ok()).unwrap_or_default();
        let libs = get(format!("{}/Library/VirtualFolders", self.url));
        let folders = get(format!("{}/Items?recursive=true&includeItemTypes=Folder&fields=Path&enableImages=false&enableUserData=false", self.url));
        closest_folder(&libs, &folders, remote)
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

    /// 自動修正媒體庫設定：媒體庫資料夾所在的「家庭影片和照片」媒體庫開啟即時監控、擷取章節圖片、時間軸預覽縮圖；
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
        crate::info!("[Jellyfin] 開啟媒體庫選項：{}", names.join("、"));
        Ok(trf!("已開啟「{}」的即時監控、擷取章節圖片、時間軸預覽縮圖", "Turned on real-time monitoring, chapter images and trickplay for “{}”", names.join("、")))
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

    /// Jellyfin 裡檔名是 file_name 的影片編號（沒有時空的）：先用名稱搜尋（只回傳幾筆），
    /// 找不到（標題改過、名稱有特殊字元）才列出全部影片比對檔名
    pub fn find(&self, file_name: &str) -> Result<Vec<String>, String> {
        let quick = self.find_quick(file_name)?;
        if !quick.is_empty() {
            return Ok(quick);
        }
        let url = format!("{}/Items?recursive=true&includeItemTypes=Movie,Video,MusicVideo,Episode&fields=Path&enableImages=false&enableUserData=false", self.url);
        let body = self.call(agent().get(&url))?.into_string().map_err(|e| e.to_string())?;
        Ok(items_with_file(&body, file_name))
    }

    /// 用名稱搜尋（剛掃到的影片名稱就是檔名去掉副檔名）
    fn find_quick(&self, file_name: &str) -> Result<Vec<String>, String> {
        let stem = file_name.rsplit_once('.').map_or(file_name, |(s, _)| s);
        let r = agent()
            .get(&format!("{}/Items", self.url))
            .query("searchTerm", stem)
            .query("recursive", "true")
            .query("includeItemTypes", "Movie,Video,MusicVideo,Episode")
            .query("fields", "Path")
            .query("enableImages", "false")
            .query("enableUserData", "false")
            .query("limit", "50");
        let body = self.call(r)?.into_string().map_err(|e| e.to_string())?;
        Ok(items_with_file(&body, file_name))
    }

    /// Jellyfin 上全部影片的索引（沒選媒體庫資料夾時「已在 Jellyfin」的標籤用）：
    /// 小寫檔名 → (位置「媒體庫名稱/family/2024」, 大小)
    pub fn file_index(&self) -> Result<std::collections::HashMap<String, Vec<(String, u64)>>, String> {
        const PAGE: usize = 2000;
        let libs = self.call(agent().get(&format!("{}/Library/VirtualFolders", self.url)))?.into_string().map_err(|e| e.to_string())?;
        let mut map: std::collections::HashMap<String, Vec<(String, u64)>> = Default::default();
        // 分批讀（影片上萬支時一次全部回傳要很久、可能逾時）
        for start in (0..).step_by(PAGE) {
            let url = format!(
                "{}/Items?recursive=true&includeItemTypes=Movie,Video,MusicVideo,Episode&fields=Path,MediaSources&enableImages=false&enableUserData=false&startIndex={start}&limit={PAGE}",
                self.url
            );
            let items = self.call(agent_long().get(&url))?.into_string().map_err(|e| e.to_string())?;
            let (page, total) = parse_items(&items);
            for (k, v) in index_from_items(&libs, &items) {
                map.entry(k).or_default().extend(v);
            }
            if page.is_empty() || start + PAGE >= total {
                break;
            }
        }
        Ok(map)
    }

    /// 請 Jellyfin 掃描後等它掃到這支影片（每 2 秒問一次，最多等 wait）：回傳影片編號；時間到還沒有時 None。
    /// parent：檔案所在的 Jellyfin 資料夾（只看它裡面的，影片多時快很多；每 5 次還是看全部一次，以防那個資料夾查不到）。
    /// tick：每次問完還沒有時，告訴它等了多久
    pub fn wait_for(&self, file_name: &str, wait: Duration, parent: Option<&str>, tick: impl Fn(Duration)) -> Result<Option<String>, String> {
        let start = std::time::Instant::now();
        for n in 0u32.. {
            // 平常只問一小部分（那個資料夾、或用名稱搜尋）；每 5 次比對全部一次，以防查不到
            let ids = match (parent, n % 5 == 4) {
                (_, true) => self.find(file_name)?,
                (Some(p), false) => self.find_under(file_name, p).or_else(|_| self.find_quick(file_name))?,
                (None, false) => self.find_quick(file_name)?,
            };
            if let Some(id) = ids.into_iter().next() {
                return Ok(Some(id));
            }
            if start.elapsed() >= wait {
                break;
            }
            tick(start.elapsed());
            std::thread::sleep(Duration::from_secs(2).min(wait));
        }
        Ok(None)
    }

    /// 同 find，只看 Jellyfin 資料夾 parent 裡面的（含子資料夾）
    fn find_under(&self, file_name: &str, parent: &str) -> Result<Vec<String>, String> {
        let url = format!("{}/Items?parentId={parent}&recursive=true&includeItemTypes=Movie,Video,MusicVideo,Episode&fields=Path&enableImages=false&enableUserData=false", self.url);
        let body = self.call(agent().get(&url))?.into_string().map_err(|e| e.to_string())?;
        Ok(items_with_file(&body, file_name))
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

    /// 換掉人物的照片（人物的主要圖片；不像影片封面那樣補橫幅圖、縮圖）
    pub fn set_person_photo(&self, person: &str, jpeg: &[u8]) -> Result<(), String> {
        self.set_image(person, "Primary", jpeg)
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

    /// 移除這支影片在 Jellyfin 的封面（橫幅圖、縮圖也移除；本來就沒有的不算錯）
    pub fn remove_cover(&self, id: &str) -> Result<(), String> {
        for kind in ["Primary", "Backdrop", "Thumb"] {
            match self.auth(agent().delete(&format!("{}/Items/{id}/Images/{kind}", self.url))).call() {
                Ok(_) | Err(ureq::Error::Status(404, _)) => {}
                Err(e) if kind == "Primary" => return Err(describe(&e)),
                Err(e) => crate::info!("[Jellyfin] 移除 {kind} 圖失敗：{}", describe(&e)),
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
    /// 媒體庫（「合集」「播放清單」不列：瀏覽頁另外有這兩格）
    pub fn views(&self, user: &str) -> Result<Vec<JfItem>, String> {
        let body = self.call(agent().get(&format!("{}/Users/{user}/Views", self.url)))?.into_string().map_err(|e| e.to_string())?;
        let skip = list_views(&body);
        Ok(parse_items(&body).0.into_iter().filter(|i| !skip.contains(&i.id)).map(|i| JfItem { kind: JfKind::Folder, ..i }).collect())
    }

    /// 伺服器上的合集或（這個使用者的）播放清單，依名稱排
    pub fn lists(&self, user: &str, kind: JfList) -> Result<Vec<JfItem>, String> {
        let req = self
            .items_req(&format!("{}/Users/{user}/Items", self.url))
            .query("recursive", "true")
            .query("includeItemTypes", kind.item_type())
            .query("sortBy", "SortName")
            .query("sortOrder", "Ascending");
        let body = self.call(req)?.into_string().map_err(|e| e.to_string())?;
        Ok(parse_items(&body).0.into_iter().map(|i| JfItem { kind: JfKind::Folder, ..i }).collect())
    }

    /// 新建一個合集或播放清單，放進這些影片。回傳新的編號
    pub fn create_list(&self, user: &str, kind: JfList, name: &str, ids: &[String]) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err(tr!("名稱不能空白", "The name can't be empty").into());
        }
        let req = match kind {
            JfList::Collection => agent().post(&format!("{}/Collections", self.url)).query("name", name).query("ids", &ids.join(",")),
            JfList::Playlist => agent().post(&format!("{}/Playlists", self.url)).query("name", name).query("ids", &ids.join(",")).query("userId", user).query("mediaType", "Video"),
        };
        let body = self.call(req)?.into_string().map_err(|e| e.to_string())?;
        serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|v| v.get("Id").and_then(Value::as_str).map(String::from))
            .ok_or_else(|| tr!("Jellyfin 沒有回答新的編號", "Jellyfin didn't return the new id").to_string())
    }

    /// 把影片加進合集或播放清單（合集裡已經有的 Jellyfin 不會重複放）
    pub fn add_to_list(&self, user: &str, kind: JfList, list: &str, ids: &[String]) -> Result<(), String> {
        let req = match kind {
            JfList::Collection => agent().post(&format!("{}/Collections/{list}/Items", self.url)).query("ids", &ids.join(",")),
            JfList::Playlist => agent().post(&format!("{}/Playlists/{list}/Items", self.url)).query("ids", &ids.join(",")).query("userId", user),
        };
        self.call(req).map(|_| ())
    }

    /// 從合集或播放清單移除（影片本身不刪）：合集用影片編號，播放清單用清單裡那一筆的編號（JfItem::entry）
    pub fn remove_from_list(&self, kind: JfList, list: &str, items: &[JfItem]) -> Result<(), String> {
        let req = match kind {
            JfList::Collection => agent().delete(&format!("{}/Collections/{list}/Items", self.url)).query("ids", &items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>().join(",")),
            JfList::Playlist => agent().delete(&format!("{}/Playlists/{list}/Items", self.url)).query("entryIds", &items.iter().map(|i| i.entry.as_str()).collect::<Vec<_>>().join(",")),
        };
        self.call(req).map(|_| ())
    }

    /// 播放清單裡的影片（照清單的順序）：回傳（這一頁, 全部幾個）
    pub fn playlist_items(&self, user: &str, list: &str, start: usize, limit: usize) -> Result<(Vec<JfItem>, usize), String> {
        let req = self
            .items_req(&format!("{}/Playlists/{list}/Items", self.url))
            .query("userId", user)
            .query("startIndex", &start.to_string())
            .query("limit", &limit.to_string())
            .query("enableTotalRecordCount", "true");
        let body = self.call(req)?.into_string().map_err(|e| e.to_string())?;
        Ok(parse_items(&body))
    }

    /// 一個資料夾（媒體庫、合集）裡的東西：資料夾在前，影片依 sort 排；search 不是空的時找整個伺服器。
    /// filter 不是「全部」時列出這個資料夾底下（含子資料夾）符合的影片；facets（人物、標籤、年份）有選時找這個資料夾底下
    /// （含子資料夾；parent 是 None 時整個伺服器）的影片（同一類任一個符合、不同類都要符合）。回傳（這一頁, 全部幾個）
    #[allow(clippy::too_many_arguments)]
    pub fn items(&self, user: &str, parent: Option<&str>, search: &str, start: usize, limit: usize, sort: JfSort, filter: JfFilter, facets: &[JfFacet]) -> Result<(Vec<JfItem>, usize), String> {
        let mut req = self.items_req(&format!("{}/Users/{user}/Items", self.url)).query("startIndex", &start.to_string()).query("limit", &limit.to_string()).query("enableTotalRecordCount", "true");
        let (by, order) = sort.query();
        let search = search.trim();
        let lacking: Vec<NameKind> = facets
            .iter()
            .filter_map(|f| match f {
                JfFacet::NoPeople => Some(NameKind::Person),
                JfFacet::NoTags => Some(NameKind::Tag),
                _ => None,
            })
            .collect();
        if !lacking.is_empty() {
            return self.items_lacking(user, parent, start, limit, sort, filter, facets, &lacking);
        }
        if !facets.is_empty() {
            req = req.query("sortBy", by).query("sortOrder", order).query("recursive", "true").query("includeItemTypes", VIDEO_TYPES);
            for (k, v) in facet_query(facets) {
                req = req.query(k, &v);
            }
        } else if !search.is_empty() {
            req = req.query("searchTerm", search).query("recursive", "true").query("includeItemTypes", "Video,Movie,Episode,MusicVideo,Folder,BoxSet");
        } else if filter == JfFilter::All {
            req = req.query("sortBy", &format!("IsFolder,{by}")).query("sortOrder", &format!("Descending,{order}"));
        } else {
            req = req.query("sortBy", by).query("sortOrder", order).query("recursive", "true").query("includeItemTypes", VIDEO_TYPES);
        }
        if search.is_empty() {
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

    /// 「依這些找」有「沒有人物」「沒有標籤」時：其他條件交給伺服器，讀全部符合的（太多時到 2 萬支為止）、
    /// 留下沒有那些的，再切出這一頁；parent 有給時只找那個資料夾（含子資料夾）。回傳（這一頁, 全部幾個）
    #[allow(clippy::too_many_arguments)]
    fn items_lacking(
        &self,
        user: &str,
        parent: Option<&str>,
        start: usize,
        limit: usize,
        sort: JfSort,
        filter: JfFilter,
        facets: &[JfFacet],
        lacking: &[NameKind],
    ) -> Result<(Vec<JfItem>, usize), String> {
        let (by, order) = sort.query();
        // 換頁時不用整個資料夾再讀一次：上一次的結果 60 秒內直接用（改了影片資訊會清掉，見 forget_lacking）
        let key = format!("{}|{user}|{parent:?}|{sort:?}|{filter:?}|{facets:?}|{lacking:?}", self.url);
        if let Some((_, _, all)) = LACKING.lock().unwrap_or_else(|e| e.into_inner()).as_ref().filter(|(k, at, _)| *k == key && at.elapsed() < Duration::from_secs(60)) {
            let page: Vec<Value> = all.iter().skip(start).take(limit).cloned().collect();
            return Ok((parse_items(&serde_json::json!({ "Items": page }).to_string()).0, all.len()));
        }
        let mut keep: Vec<Value> = vec![];
        let mut at = 0usize;
        loop {
            let mut req = self
                .items_req(&format!("{}/Users/{user}/Items", self.url))
                .query("recursive", "true")
                .query("includeItemTypes", VIDEO_TYPES)
                .query("sortBy", by)
                .query("sortOrder", order)
                .query("startIndex", &at.to_string())
                .query("limit", "500")
                .query("enableTotalRecordCount", "true");
            for (k, v) in facet_query(facets) {
                req = req.query(k, &v);
            }
            if let Some(p) = parent {
                req = req.query("parentId", p);
            }
            if let Some(f) = filter.query() {
                req = req.query("filters", f);
            }
            let body = self.call(req)?.into_string().map_err(|e| e.to_string())?;
            let (got, total) = lacking_in(&body, lacking, &mut keep);
            at += got;
            if got == 0 || at >= total || at >= 20_000 {
                break;
            }
        }
        let n = keep.len();
        let page: Vec<Value> = keep.iter().skip(start).take(limit).cloned().collect();
        *LACKING.lock().unwrap_or_else(|e| e.into_inner()) = Some((key, std::time::Instant::now(), std::sync::Arc::new(keep)));
        Ok((parse_items(&serde_json::json!({ "Items": page }).to_string()).0, n))
    }

    /// 清單要的欄位、縮圖
    fn items_req(&self, url: &str) -> ureq::Request {
        agent().get(url).query("fields", ITEM_FIELDS).query("enableImageTypes", "Primary").query("imageTypeLimit", "1")
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
        crate::jf_proxy::url(&self.url, &self.key, &format!("/Videos/{id}/{id}/Subtitles/{index}/0/Stream.srt"))
    }

    /// 把原檔下載到 to（先寫暫存檔、顯示進度、可以取消）；回傳幾個位元組
    pub fn download(&self, id: &str, to: &std::path::Path) -> Result<u64, String> {
        let r = self.auth(agent_long().get(&format!("{}/Videos/{id}/stream", self.url)).query("static", "true")).call().map_err(|e| describe(&e))?;
        let total = r.header("Content-Length").and_then(|n| n.parse().ok()).unwrap_or(0);
        if let Some(dir) = to.parent() {
            std::fs::create_dir_all(dir).map_err(|e| crate::error::plain(&e))?;
        }
        crate::transfer::save(r.into_reader(), total, to, true).map_err(|e| e.to_string())
    }

    /// 串流播放的網址（原檔直接傳過來，不在 NAS 上轉檔；可以跳著看）
    pub fn stream_url(&self, id: &str) -> String {
        crate::jf_proxy::url(&self.url, &self.key, &format!("/Videos/{id}/stream?static=true"))
    }

    /// 幫一支影片放上字幕（語音辨識做好的 SRT）：先用 Jellyfin 的上傳字幕（POST /Videos/{id}/Subtitles）；
    /// 伺服器不讓上傳（舊版、權限）而這台電腦有選媒體庫資料夾時，放到那一份影片旁邊（同名.語言.srt）再請 Jellyfin 重新讀取。
    /// lang 是 Jellyfin 的語言（zh-TW、eng…）。回傳放在哪裡（給使用者看）
    pub fn put_subtitle(&self, item: &JfItem, lang: &str, srt: &str) -> Result<String, String> {
        let body = serde_json::json!({ "Language": lang, "Format": "srt", "IsForced": false, "IsHearingImpaired": false, "Data": crate::steps::base64(srt.as_bytes()) }).to_string();
        let upload = self.call_body(agent().post(&format!("{}/Videos/{}/Subtitles", self.url, item.id)), &body);
        let Err(e) = upload else { return Ok(tr!("傳到 Jellyfin", "uploaded to Jellyfin").into()) };
        crate::info!("[Jellyfin] 上傳字幕失敗，改放到媒體庫資料夾：{e}");
        let local = self.server_root().and_then(|r| local_path(&r, &self.root, &item.path)).filter(|p| p.exists());
        let Some(video) = local else {
            return Err(trf!(
                "Jellyfin 不讓上傳字幕（{e}）。在「設定 → 存檔與分享 → Jellyfin」選好媒體庫資料夾的話，可以改放到影片旁邊",
                "Jellyfin didn't accept the subtitles ({e}). If you choose the library folder in Settings → Saving & sharing → Jellyfin, they can be saved next to the video instead"
            ));
        };
        let stem = video.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let out = video.with_file_name(format!("{stem}.{lang}.srt"));
        std::fs::write(&out, srt).map_err(|e| trf!("字幕寫不進媒體庫資料夾：{e}", "Couldn't write the subtitles to the library folder: {e}"))?;
        let url = format!("{}/Items/{}/Refresh?metadataRefreshMode=Default&imageRefreshMode=Default&replaceAllMetadata=false&replaceAllImages=false", self.url, item.id);
        self.call(agent().post(&url))?;
        Ok(trf!("放到媒體庫資料夾（{}）", "saved in the library folder ({})", out.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()))
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

    /// 全部影片用到的人物、標籤、年份與次數（「影片資訊」的常用、Jellyfin 分頁「依這些找」後面的支數）。
    /// 人物、標籤依次數多到少、再依名稱排，只差在前後空白、大小寫、全形半形的算同一個，顯示用得最多的寫法；
    /// 年份新的在前；太多時讀到 2 萬支為止
    pub fn usage(&self, user: &str) -> Result<Usage, String> {
        self.usage_in(user, None)
    }

    /// 同 usage，parent 有給時只算那個資料夾（含子資料夾）裡的影片（Jellyfin 分頁在某個資料夾裡「依這些找」的支數）
    pub fn usage_in(&self, user: &str, parent: Option<&str>) -> Result<Usage, String> {
        let (mut people, mut tags, mut years) = (vec![], vec![], vec![]);
        let (mut no_people, mut no_tags) = (0usize, 0usize);
        let mut start = 0usize;
        loop {
            let mut req = agent()
                .get(&format!("{}/Users/{user}/Items", self.url))
                .query("recursive", "true")
                .query("includeItemTypes", VIDEO_TYPES)
                .query("fields", "People,Tags")
                .query("enableImages", "false")
                .query("enableUserData", "false")
                .query("startIndex", &start.to_string())
                .query("limit", "1000")
                .query("enableTotalRecordCount", "true");
            if let Some(p) = parent {
                req = req.query("parentId", p);
            }
            let body = self.call(req)?.into_string().map_err(|e| e.to_string())?;
            let (p, t, y, got, total) = people_tags_of_items(&body);
            let (np, nt) = lacking_counts(&body);
            no_people += np;
            no_tags += nt;
            people.extend(p);
            tags.extend(t);
            years.extend(y);
            start += got;
            if got == 0 || start >= total || start >= 20_000 {
                break;
            }
        }
        Ok(Usage { people: tally(people), tags: tally(tags), years: tally_years(years), no_people, no_tags })
    }

    /// 把全部影片裡的一個人物或標籤改成另一個寫法（「常用」裡寫錯的改正確；改成已經有的就是合併）。
    /// from 只差前後空白、大小寫、全形半形的寫法一起改；一支一支取回整筆資料換上去再存，其他資訊不動。
    /// progress(改好幾支, 全部幾支)；有幾支失敗時其他照改，最後回報失敗的。回傳改了幾支
    /// progress 回傳 false 就停（取消；改好的保留，再改一次會接著改完剩下的）
    pub fn rename_name(&self, user: &str, kind: NameKind, from: &str, to: &str, progress: &dyn Fn(usize, usize) -> bool) -> Result<usize, String> {
        forget_lacking();
        let to = to.trim();
        if to.is_empty() {
            return Err(tr!("新的寫法不能空白", "The new spelling can't be empty").into());
        }
        let ids = self.items_using(user, kind, from)?;
        let total = ids.len();
        progress(0, total);
        let (mut done, mut failed) = (0usize, vec![]);
        for (i, id) in ids.iter().enumerate() {
            if i > 0 && !progress(i, total) {
                break;
            }
            let r = self.change_item(user, id, |body| with_renamed(body, kind, from, to));
            match r {
                Ok(()) => done += 1,
                Err(e) => {
                    crate::info!("[Jellyfin] 重新命名「{from}」→「{to}」，影片 {id} 沒改到：{e}");
                    failed.push(e);
                }
            }
            progress(i + 1, total);
        }
        match failed.first() {
            None => Ok(done),
            Some(e) => Err(trf!("改好 {done} 支，{} 支沒改到：{e}", "Changed {done}, {} failed: {e}", failed.len())),
        }
    }

    /// 好幾支影片一起加、移除人物和標籤、改年份（勾選好幾支 →「整理人物、標籤…」）：一支一支取回整筆資料、
    /// 只換這次改的再存回去。progress(改好幾支, 全部幾支) 回傳 false 就停（取消；已經改的不還原）；
    /// 有幾支失敗時其他照改，最後回報失敗的。回傳改了幾支
    pub fn edit_many(&self, user: &str, ids: &[String], edit: &BatchEdit, progress: &dyn Fn(usize, usize) -> bool) -> Result<usize, String> {
        let total = ids.len();
        progress(0, total);
        let (mut done, mut failed) = (0usize, vec![]);
        for (i, id) in ids.iter().enumerate() {
            if i > 0 && !progress(i, total) {
                break;
            }
            match self.change_item(user, id, |body| with_batch(body, edit)) {
                Ok(()) => done += 1,
                Err(e) => {
                    crate::info!("[Jellyfin] 整理影片 {id} 沒改到：{e}");
                    failed.push(e);
                }
            }
            progress(i + 1, total);
        }
        match failed.first() {
            None => Ok(done),
            Some(e) => Err(trf!("改好 {done} 支，{} 支沒改到：{e}", "Changed {done}, {} failed: {e}", failed.len())),
        }
    }

    /// 取回一支影片的整筆資料，用 f 換好再存回去（POST /Items/{id}）
    fn change_item(&self, user: &str, id: &str, f: impl FnOnce(&str) -> Option<String>) -> Result<(), String> {
        forget_lacking();
        let body = match self.call(agent().get(&format!("{}/Items/{id}", self.url))) {
            Ok(r) => r.into_string().map_err(|e| e.to_string())?,
            Err(_) => self.call(agent().get(&format!("{}/Users/{user}/Items/{id}", self.url)))?.into_string().map_err(|e| e.to_string())?,
        };
        let item = f(&body).ok_or_else(|| tr!("Jellyfin 的影片資料看不懂", "Couldn't understand Jellyfin's item data").to_string())?;
        self.call_body(agent().post(&format!("{}/Items/{id}", self.url)), &item).map(|_| ())
    }

    /// 用到這個人物（演員）或標籤的影片編號（只差前後空白、大小寫、全形半形的算同一個）；太多時讀到 2 萬支為止
    fn items_using(&self, user: &str, kind: NameKind, name: &str) -> Result<Vec<String>, String> {
        let key = name_key(name);
        let mut out = vec![];
        let mut start = 0usize;
        loop {
            let req = agent()
                .get(&format!("{}/Users/{user}/Items", self.url))
                .query("recursive", "true")
                .query("includeItemTypes", VIDEO_TYPES)
                .query("fields", "People,Tags")
                .query("enableImages", "false")
                .query("enableUserData", "false")
                .query("startIndex", &start.to_string())
                .query("limit", "1000")
                .query("enableTotalRecordCount", "true");
            let body = self.call(req)?.into_string().map_err(|e| e.to_string())?;
            let Ok(v) = serde_json::from_str::<Value>(&body) else { break };
            let items = v.get("Items").and_then(Value::as_array).cloned().unwrap_or_default();
            for i in &items {
                if names_in(i, kind).iter().any(|n| name_key(n) == key) {
                    if let Some(id) = i.get("Id").and_then(Value::as_str) {
                        out.push(id.to_string());
                    }
                }
            }
            let total = v.get("TotalRecordCount").and_then(Value::as_u64).unwrap_or(0) as usize;
            start += items.len();
            if items.is_empty() || start >= total || start >= 20_000 {
                break;
            }
        }
        Ok(out)
    }

    /// 影片用到的標籤、年份（Jellyfin 分頁「依標籤 / 年份找」）。先問 /Items/Filters；
    /// 有些伺服器那裡回空的（或沒有這個功能），就一頁一頁讀全部影片的標籤、年份自己整理
    pub fn facets(&self, user: &str) -> Result<(Vec<String>, Vec<u32>), String> {
        let req = agent().get(&format!("{}/Items/Filters", self.url)).query("userId", user).query("includeItemTypes", VIDEO_TYPES).query("recursive", "true");
        let quick = self.call(req).ok().and_then(|r| r.into_string().ok()).map(|b| facets_of(&b)).unwrap_or_default();
        if !quick.0.is_empty() && !quick.1.is_empty() {
            return Ok(quick);
        }
        let (mut tags, mut years) = quick;
        let mut start = 0usize;
        loop {
            let req = agent()
                .get(&format!("{}/Users/{user}/Items", self.url))
                .query("recursive", "true")
                .query("includeItemTypes", VIDEO_TYPES)
                .query("fields", "Tags")
                .query("enableImages", "false")
                .query("enableUserData", "false")
                .query("startIndex", &start.to_string())
                .query("limit", "1000")
                .query("enableTotalRecordCount", "true");
            let body = self.call(req)?.into_string().map_err(|e| e.to_string())?;
            let (t, y, got, total) = tags_years_of_items(&body);
            tags.extend(t);
            years.extend(y);
            start += got;
            // 讀完了（太多時讀到 2 萬支為止）
            if got == 0 || start >= total || start >= 20_000 {
                break;
            }
        }
        Ok(tidy_facets(tags, years))
    }

    /// 一支影片的資訊（標題、說明、人物、標籤、年份），在檔案庫 Jellyfin 分頁修改用
    pub fn item_info(&self, user: &str, id: &str) -> Result<JfInfo, String> {
        let body = self.call(agent().get(&format!("{}/Users/{user}/Items/{id}", self.url)))?.into_string().map_err(|e| e.to_string())?;
        info_of(&body).ok_or_else(|| tr!("Jellyfin 的影片資料看不懂", "Couldn't understand Jellyfin's item data").to_string())
    }

    /// 一支影片的演員與它們在伺服器上的編號（人物照片用）：直接問伺服器，剛在「影片資訊」加的人物也有編號
    pub fn item_people(&self, user: &str, id: &str) -> Result<Vec<(Option<String>, String)>, String> {
        let body = self.call(agent().get(&format!("{}/Users/{user}/Items/{id}", self.url)))?.into_string().map_err(|e| e.to_string())?;
        people_ids_of(&body).ok_or_else(|| tr!("Jellyfin 的影片資料看不懂", "Couldn't understand Jellyfin's item data").to_string())
    }

    /// 把改好的資訊存回去：取回整筆資料、換上去再存（清空的欄位就清掉；不是演員的人物，例如導演，留著）
    pub fn set_info(&self, user: &str, id: &str, info: &JfInfo) -> Result<(), String> {
        forget_lacking();
        let body = match self.call(agent().get(&format!("{}/Items/{id}", self.url))) {
            Ok(r) => r.into_string().map_err(|e| e.to_string())?,
            Err(_) => self.call(agent().get(&format!("{}/Users/{user}/Items/{id}", self.url)))?.into_string().map_err(|e| e.to_string())?,
        };
        let item = with_info(&body, info).ok_or_else(|| tr!("Jellyfin 的影片資料看不懂", "Couldn't understand Jellyfin's item data").to_string())?;
        // 存檔時 Jellyfin 要寫中繼資料、伺服器忙（例如剛讀完一頁縮圖）時可能超過 15 秒：等久一點
        self.call_body(agent_long().post(&format!("{}/Items/{id}", self.url)), &item).map(|_| ())
    }

    /// 從 Jellyfin 刪掉這支影片（伺服器上的檔案也一起刪，不會進資源回收筒）
    pub fn delete_item(&self, id: &str) -> Result<(), String> {
        forget_lacking();
        match self.auth(agent().delete(&format!("{}/Items/{id}", self.url))).call() {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(401 | 403, _)) => {
                Err(tr!("Jellyfin 不允許刪除：到 Jellyfin 控制台 → 使用者，開啟「允許刪除媒體」", "Jellyfin doesn't allow deleting: in the Jellyfin Dashboard → Users, turn on “Allow media deletion”")
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

    /// 這支影片（編號）加進或拿出 user 的我的最愛
    pub fn set_favorite_item(&self, user: &str, id: &str, on: bool) -> Result<(), String> {
        let url = format!("{}/Users/{user}/FavoriteItems/{id}", self.url);
        self.call(if on { agent().post(&url) } else { agent().delete(&url) }).map(|_| ())
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

    /// 有沒有任何一個媒體庫開啟了「時間軸預覽縮圖」（Trickplay）
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
        resend(req, Some(&crate::steps::base64(jpeg))).map(|_| ())
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

    /// 請 Jellyfin 產生章節縮圖（執行「擷取章節圖片」排程工作）。回傳媒體庫有沒有開啟「擷取章節圖片」（沒開啟時工作不會做）
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
        // 沒有檔名（不應該發生）：不等一支找不到的影片
        if file_name.trim().is_empty() && ids.is_empty() {
            return String::new();
        }
        let stage = |what: &str| crate::transfer::set_stage(file_name, Some(what.to_string()));
        let ids = if ids.is_empty() {
            stage(tr!("等 Jellyfin 掃描到這支", "Waiting for Jellyfin to find it"));
            let remote = local.and_then(|l| self.remote_dir(l.parent()?));
            // 放進去的資料夾不在任何媒體庫裡：Jellyfin 永遠掃不到，直接說，不要白等
            if let Some((r, locs)) = remote.as_deref().and_then(|r| self.outside(r).map(|l| (r, l))) {
                crate::transfer::set_stage(file_name, None);
                return trf!(
                    "（Jellyfin 的媒體庫沒有包含這個資料夾（{r}），Jellyfin 看不到這支影片：請在 Jellyfin 控制台把它加進媒體庫，或傳到媒體庫裡的資料夾：{}）",
                    " (No Jellyfin library includes this folder ({r}), so Jellyfin can't see this video. Add it to a library in the Jellyfin Dashboard, or send to a folder inside one: {})",
                    locs.join(tr!("、", ", "))
                );
            }
            let parent = remote.and_then(|r| self.folder_for(&r)).map(|(id, _)| id);
            let tick = |waited: Duration| {
                if waited >= Duration::from_secs(20) {
                    let n = crate::format::human_duration(waited.as_secs() as f64);
                    stage(&trf!("等 Jellyfin 掃描到這支（已等 {n}）", "Waiting for Jellyfin to find it ({n} so far)"));
                }
            };
            let found = self.wait_for(file_name, wait, parent.as_deref(), tick);
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
        // 時間軸預覽縮圖（10.9 以後）：媒體庫有開啟才做，沒開啟時設定裡的「自動修正」會開啟。做的時候畫面右下角顯示進度
        if self.trickplay_enabled() && self.run_task("RefreshTrickplayImages").is_ok() {
            notes.push(tr!("正在做時間軸預覽縮圖", "making trickplay previews").into());
            let (jf, name) = (self.clone(), file_name.to_string());
            std::thread::spawn(move || jf.watch_task("RefreshTrickplayImages", &name, tr!("做時間軸預覽縮圖", "Making trickplay previews")));
        }
        if extras.chapters {
            notes.push(match self.chapter_images() {
                Ok(true) => tr!("正在做章節縮圖", "making chapter thumbnails").into(),
                Ok(false) => tr!("要做章節縮圖請在 Jellyfin 的媒體庫設定開啟「擷取章節圖片」", "turn on “Extract chapter images” in the Jellyfin library settings for chapter thumbnails").into(),
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
        resend(self.auth(req), None)
    }

    fn call_body(&self, req: ureq::Request, json: &str) -> Result<ureq::Response, String> {
        resend(self.auth(req).set("Content-Type", "application/json"), Some(json))
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
    // 只給大人看：R 級（兒童帳號設最高分級就看不到）；移除時清掉我們設的 R
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

/// 有沒有任何一個媒體庫開啟了「擷取章節圖片」
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

/// 伺服器上的路徑在這台電腦的媒體庫資料夾裡是哪裡（server_path 的反方向）；不在媒體庫資料夾底下時 None
pub fn local_path(server_root: &str, local_root: &str, server: &str) -> Option<std::path::PathBuf> {
    let norm = |s: &str| s.replace('\\', "/").trim_end_matches('/').to_string();
    let (root, path) = (norm(server_root.trim()), norm(server));
    if root.is_empty() || local_root.trim().is_empty() || path.len() <= root.len() || !path.is_char_boundary(root.len()) || !path[..root.len()].eq_ignore_ascii_case(&root) {
        return None;
    }
    let rest = &path[root.len()..];
    if !rest.starts_with('/') {
        return None;
    }
    let mut out = std::path::PathBuf::from(local_root.trim());
    for part in rest.split('/').filter(|p| !p.is_empty()) {
        // 伺服器給的路徑不能跑到媒體庫資料夾外面（..）、不能是磁碟機或檔案的其他資料流（:）
        if part == "." || part == ".." || part.contains(':') {
            return None;
        }
        out.push(part);
    }
    Some(out)
}

/// 語音辨識的語言（zh、en、auto）在 Jellyfin 字幕上的語言
pub fn subtitle_lang(whisper: &str) -> &'static str {
    match whisper {
        "zh" => "zh-TW",
        "en" => "eng",
        _ => "und",
    }
}

/// Jellyfin 上的一個東西：資料夾（媒體庫、資料夾、合集）或影片
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JfKind {
    #[default]
    Folder,
    Video,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct JfItem {
    pub id: String,
    pub name: String,
    pub kind: JfKind,
    /// 影片的檔名（播放時判斷格式用）；資料夾是空的
    pub file_name: String,
    /// 影片在伺服器上的完整路徑（字幕傳不上去時找媒體庫資料夾裡的那一份用）
    pub path: String,
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
    /// 在這個使用者的我的最愛裡
    pub favorite: bool,
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
    /// 人物（演員）、標籤（勾選好幾支一起整理、清單檢視用）
    pub people: Vec<String>,
    pub tags: Vec<String>,
    /// 年份（ProductionYear；沒設時 None）
    pub year: Option<i32>,
    /// 檔案大小（位元組；不知道時 0）
    pub size: u64,
    /// 在播放清單裡時，這一筆在清單裡的編號（從播放清單移除用；同一支影片可以在清單裡好幾次）；其他時候空的
    pub entry: String,
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

/// Jellyfin 分頁「依這些找」的一個條件：人物（編號, 名稱）、標籤、年份
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JfFacet {
    Person(String, String),
    Tag(String),
    Year(u32),
    /// 還沒設人物、還沒有標籤的影片（Jellyfin 沒有這種查詢，讀全部再自己挑）
    NoPeople,
    NoTags,
}

/// 在檔案庫 Jellyfin 分頁修改的影片資訊
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

/// 瀏覽清單要的欄位
const ITEM_FIELDS: &str = "Path,MediaStreams,MediaSources,DateCreated,PremiereDate,ChildCount,Chapters,Trickplay,People,Tags";

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
    /// 名稱倒過來（Z→A）
    NameDesc,
    /// 最長的在前、最短的在前
    Longest,
    Shortest,
}

impl JfSort {
    pub const ALL: [JfSort; 7] = [JfSort::Newest, JfSort::Oldest, JfSort::Added, JfSort::Name, JfSort::NameDesc, JfSort::Longest, JfSort::Shortest];

    pub fn label(self) -> &'static str {
        match self {
            JfSort::Newest => tr!("最新的在前", "Newest first"),
            JfSort::Oldest => tr!("最舊的在前", "Oldest first"),
            JfSort::Added => tr!("最近加入的在前", "Recently added"),
            JfSort::Name => tr!("依名稱", "By name"),
            JfSort::NameDesc => tr!("依名稱（倒過來）", "By name (Z→A)"),
            JfSort::Longest => tr!("最長的在前", "Longest first"),
            JfSort::Shortest => tr!("最短的在前", "Shortest first"),
        }
    }

    fn query(self) -> (&'static str, &'static str) {
        match self {
            JfSort::Newest => ("PremiereDate,DateCreated,SortName", "Descending,Descending,Ascending"),
            JfSort::Oldest => ("PremiereDate,DateCreated,SortName", "Ascending,Ascending,Ascending"),
            JfSort::Added => ("DateCreated,SortName", "Descending,Ascending"),
            JfSort::Name => ("SortName", "Ascending"),
            JfSort::NameDesc => ("SortName", "Descending"),
            JfSort::Longest => ("Runtime,SortName", "Descending,Ascending"),
            JfSort::Shortest => ("Runtime,SortName", "Ascending,Ascending"),
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
    crate::log::redact(text)
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
                        path: i.get("Path").and_then(Value::as_str).unwrap_or("").to_string(),
                        duration: i.get("RunTimeTicks").and_then(Value::as_f64).unwrap_or(0.0) / 1e7,
                        width: num(video, "Width") as u32,
                        height: num(video, "Height") as u32,
                        fps: [num(video, "RealFrameRate"), num(video, "AverageFrameRate")].into_iter().find(|f| *f > 0.0).unwrap_or(30.0),
                        has_audio: streams.iter().any(|s| s.get("Type").and_then(Value::as_str) == Some("Audio")),
                        date,
                        played: i.pointer("/UserData/Played").and_then(Value::as_bool) == Some(true),
                        favorite: i.pointer("/UserData/IsFavorite").and_then(Value::as_bool) == Some(true),
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
                        people: names_in(i, NameKind::Person),
                        tags: names_in(i, NameKind::Tag),
                        year: i.get("ProductionYear").and_then(Value::as_i64).filter(|y| *y > 0).map(|y| y as i32),
                        size: i.pointer("/MediaSources/0/Size").and_then(Value::as_u64).unwrap_or(0),
                        entry: i.get("PlaylistItemId").and_then(Value::as_str).unwrap_or("").to_string(),
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

/// 影片清單（fields=Tags）裡的（標籤, 年份, 這一頁幾支, 全部幾支）
fn tags_years_of_items(json: &str) -> (Vec<String>, Vec<u32>, usize, usize) {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return (vec![], vec![], 0, 0) };
    let items = v.get("Items").and_then(Value::as_array).cloned().unwrap_or_default();
    let tags = items.iter().filter_map(|i| i.get("Tags").and_then(Value::as_array)).flatten().filter_map(|t| t.as_str().map(|s| s.trim().to_string())).filter(|t| !t.is_empty()).collect();
    let years = items.iter().filter_map(|i| i.get("ProductionYear").and_then(Value::as_u64)).filter(|y| *y > 0).map(|y| y as u32).collect();
    let total = v.get("TotalRecordCount").and_then(Value::as_u64).unwrap_or(0) as usize;
    (tags, years, items.len(), total)
}

/// （名稱, 用在幾支影片），次數多的在前
pub type Counts = Vec<(String, usize)>;

/// 全部影片用到的人物、標籤、年份各幾支
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    pub people: Counts,
    pub tags: Counts,
    /// （年份, 幾支），新的在前
    pub years: Vec<(u32, usize)>,
    /// 還沒設人物、還沒有標籤的影片幾支
    pub no_people: usize,
    pub no_tags: usize,
}

/// 影片 JSON 裡的演員（編號, 名稱）；導演等其他人物不算，沒有編號時 None
fn people_ids_of(body: &str) -> Option<Vec<(Option<String>, String)>> {
    let item: Value = serde_json::from_str(body).ok()?;
    Some(
        item.get("People")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|p| p.get("Type").and_then(Value::as_str).is_none_or(|t| t == "Actor"))
            .filter_map(|p| {
                let name = p.get("Name")?.as_str()?.trim();
                let id = p.get("Id").and_then(Value::as_str).filter(|i| !i.is_empty()).map(String::from);
                (!name.is_empty()).then(|| (id, name.to_string()))
            })
            .collect(),
    )
}

/// 「常用」重新命名時改的是人物還是標籤
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameKind {
    Person,
    Tag,
}

/// 一支影片（JSON）裡的演員或標籤
fn names_in(item: &Value, kind: NameKind) -> Vec<String> {
    match kind {
        NameKind::Person => item
            .get("People")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|p| p.get("Type").and_then(Value::as_str).is_none_or(|t| t == "Actor"))
            .filter_map(|p| p.get("Name")?.as_str().map(String::from))
            .collect(),
        NameKind::Tag => item.get("Tags").and_then(Value::as_array).into_iter().flatten().filter_map(|t| t.as_str().map(String::from)).collect(),
    }
}

/// 合集（BoxSet）或播放清單
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JfList {
    Collection,
    Playlist,
}

impl JfList {
    fn item_type(self) -> &'static str {
        match self {
            JfList::Collection => "BoxSet",
            JfList::Playlist => "Playlist",
        }
    }
}

/// /Users/{u}/Views 裡「合集」「播放清單」那兩個的編號
fn list_views(json: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return vec![] };
    v.get("Items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|i| matches!(i.get("CollectionType").and_then(Value::as_str), Some("boxsets" | "playlists")))
        .filter_map(|i| i.get("Id")?.as_str().map(String::from))
        .collect()
}

/// 一頁影片（JSON）裡（沒有人物的幾支, 沒有標籤的幾支）
fn lacking_counts(json: &str) -> (usize, usize) {
    let (mut p, mut t) = (vec![], vec![]);
    lacking_in(json, &[NameKind::Person], &mut p);
    lacking_in(json, &[NameKind::Tag], &mut t);
    (p.len(), t.len())
}

/// 一頁影片（JSON）裡 lacking 每一種都沒有的放進 keep；回傳（這一頁幾支, 全部幾支）
fn lacking_in(json: &str, lacking: &[NameKind], keep: &mut Vec<Value>) -> (usize, usize) {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return (0, 0) };
    let items = v.get("Items").and_then(Value::as_array).cloned().unwrap_or_default();
    let got = items.len();
    keep.extend(items.into_iter().filter(|i| lacking.iter().all(|k| names_in(i, *k).iter().all(|n| n.trim().is_empty()))));
    (got, v.get("TotalRecordCount").and_then(Value::as_u64).unwrap_or(0) as usize)
}

/// 好幾支影片一起整理（Jellyfin 分頁勾選好幾支 →「整理人物、標籤…」）：要加、要移除的人物和標籤，年份
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BatchEdit {
    pub add_people: Vec<String>,
    pub remove_people: Vec<String>,
    pub add_tags: Vec<String>,
    pub remove_tags: Vec<String>,
    /// None：不改；Some(None)：清掉；Some(Some(y))：都改成 y
    pub year: Option<Option<i32>>,
}

impl BatchEdit {
    pub fn is_empty(&self) -> bool {
        self.add_people.is_empty() && self.remove_people.is_empty() && self.add_tags.is_empty() && self.remove_tags.is_empty() && self.year.is_none()
    }
}

/// 把 BatchEdit 套進一支影片的整筆資料（POST /Items/{id} 用）：只加、只移除這次改的，其他人物、標籤不動；
/// 比對時只差前後空白、大小寫、全形半形的算同一個（已經有的不會再加一個）
pub fn with_batch(item_json: &str, e: &BatchEdit) -> Option<String> {
    let mut v: Value = serde_json::from_str(item_json).ok()?;
    let o = v.as_object_mut()?;
    let keys = |l: &[String]| l.iter().map(|n| name_key(n)).collect::<std::collections::HashSet<_>>();
    let (drop_tags, drop_people) = (keys(&e.remove_tags), keys(&e.remove_people));
    let mut seen = std::collections::HashSet::new();
    let mut tags: Vec<String> = o.get("Tags").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).filter(|t| !drop_tags.contains(&name_key(t))).map(String::from).collect();
    tags.extend(e.add_tags.iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()));
    tags.retain(|t| seen.insert(name_key(t)));
    o.insert("Tags".into(), Value::from(tags));
    seen.clear();
    let mut people: Vec<Value> = o
        .get("People")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|p| {
            let actor = p.get("Type").and_then(Value::as_str).is_none_or(|t| t == "Actor");
            let key = name_key(p.get("Name").and_then(Value::as_str).unwrap_or(""));
            !actor || (!drop_people.contains(&key) && seen.insert(key))
        })
        .cloned()
        .collect();
    for n in e.add_people.iter().map(|n| n.trim()).filter(|n| !n.is_empty()) {
        if seen.insert(name_key(n)) {
            people.push(serde_json::json!({ "Name": n, "Type": "Actor" }));
        }
    }
    o.insert("People".into(), Value::from(people));
    if let Some(year) = e.year {
        set_year(o, year);
    }
    serde_json::to_string(&v).ok()
}

/// 整筆資料（POST /Items/{id} 用）裡的人物（演員）或標籤 from 換成 to；換完同一個重複的只留一個。
/// 人物換成只有名字的（Jellyfin 依名字找到或建立那個人物）
pub fn with_renamed(item_json: &str, kind: NameKind, from: &str, to: &str) -> Option<String> {
    let mut v: Value = serde_json::from_str(item_json).ok()?;
    let o = v.as_object_mut()?;
    let key = name_key(from);
    let mut seen = std::collections::HashSet::new();
    match kind {
        NameKind::Tag => {
            let tags: Vec<Value> = o
                .get("Tags")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|t| if name_key(t) == key { to.to_string() } else { t.to_string() })
                .filter(|t| seen.insert(name_key(t)))
                .map(Value::from)
                .collect();
            o.insert("Tags".into(), Value::from(tags));
        }
        NameKind::Person => {
            let people: Vec<Value> = o
                .get("People")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|p| {
                    let actor = p.get("Type").and_then(Value::as_str).is_none_or(|t| t == "Actor");
                    let name = p.get("Name").and_then(Value::as_str).unwrap_or("");
                    if !actor {
                        return Some(p.clone());
                    }
                    let (name, p) = if name_key(name) == key { (to.to_string(), serde_json::json!({ "Name": to, "Type": "Actor" })) } else { (name.to_string(), p.clone()) };
                    seen.insert(name_key(&name)).then_some(p)
                })
                .collect();
            o.insert("People".into(), Value::from(people));
        }
    }
    serde_json::to_string(&v).ok()
}

/// 看不見的字元（零寬空白、方向記號、變體選擇符等）
fn invisible(c: char) -> bool {
    matches!(c, '\u{00AD}' | '\u{034F}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}')
}

/// 看起來的樣子：name_key 再去掉看不見的字元（兩個寫法這個一樣、name_key 不一樣 = 看起來一樣其實不同）
pub fn look_key(s: &str) -> String {
    name_key(&s.chars().filter(|c| !invisible(*c)).collect::<String>())
}

/// 兩個寫法看起來一樣、其實不同（多了看不見的字元）時，說明 a 差在哪裡；不是這種情況時 None
pub fn spelling_diff(a: &str, b: &str) -> Option<String> {
    if name_key(a) == name_key(b) || look_key(a) != look_key(b) {
        return None;
    }
    let hidden = |s: &str| s.chars().filter(|c| invisible(*c)).map(|c| format!("U+{:04X}", c as u32)).collect::<Vec<_>>().join(tr!("、", ", "));
    let (ha, hb) = (hidden(a), hidden(b));
    Some(if !ha.is_empty() {
        trf!("這個寫法多了看不見的字元（{ha}）", "This spelling has invisible characters ({ha})")
    } else {
        trf!("另一個寫法多了看不見的字元（{hb}）", "The other spelling has invisible characters ({hb})")
    })
}

/// 依人物、標籤、年份找時要加的查詢參數：同一類的串在一起（Jellyfin 列出任一個符合的），不同類分開（都要符合）
pub fn facet_query(facets: &[JfFacet]) -> Vec<(&'static str, String)> {
    let join = |sep: &str, f: &dyn Fn(&JfFacet) -> Option<String>| facets.iter().filter_map(f).collect::<Vec<_>>().join(sep);
    let people = join(",", &|f| if let JfFacet::Person(id, _) = f { Some(id.clone()) } else { None });
    // 標籤可能有逗號，Jellyfin 的 tags 用「|」分隔
    let tags = join("|", &|f| if let JfFacet::Tag(t) = f { Some(t.clone()) } else { None });
    let years = join(",", &|f| if let JfFacet::Year(y) = f { Some(y.to_string()) } else { None });
    [("personIds", people), ("tags", tags), ("years", years)].into_iter().filter(|(_, v)| !v.is_empty()).collect()
}

/// 每個年份幾支，新的在前
fn tally_years(years: Vec<u32>) -> Vec<(u32, usize)> {
    let mut m = std::collections::BTreeMap::<u32, usize>::new();
    for y in years {
        *m.entry(y).or_default() += 1;
    }
    m.into_iter().rev().collect()
}

/// 人物、標籤比對用的鍵：去掉前後空白、全形英數符號換成半形、不分大小寫（「無碼」和「無碼 」、「ＶＲ」和「vr」算同一個）
pub fn name_key(s: &str) -> String {
    s.trim()
        .chars()
        .map(|c| match c {
            '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
            '\u{3000}' => ' ',
            _ => c,
        })
        .collect::<String>()
        .trim()
        .to_lowercase()
}

/// 每個人物、標籤出現幾次：同一個鍵的算一起，名稱用出現最多次的寫法；依次數多到少、再依名稱排
fn tally(names: Vec<String>) -> Vec<(String, usize)> {
    let mut groups: std::collections::HashMap<String, (usize, std::collections::HashMap<String, usize>)> = std::collections::HashMap::new();
    for n in names {
        let n = n.trim().to_string();
        if n.is_empty() {
            continue;
        }
        let g = groups.entry(name_key(&n)).or_default();
        g.0 += 1;
        *g.1.entry(n).or_default() += 1;
    }
    let mut out: Vec<(String, usize)> = groups
        .into_values()
        .map(|(count, spellings)| {
            let name = spellings.into_iter().max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0))).map(|(n, _)| n).unwrap_or_default();
            (name, count)
        })
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
    out
}

/// 影片清單（fields=People,Tags）裡的（人物, 標籤, 年份, 這一頁幾支, 全部幾支）；人物只算演員（我們設的人物都是演員）。
/// 年份只看 ProductionYear（依年份找時 Jellyfin 比對的是這個）
fn people_tags_of_items(json: &str) -> (Vec<String>, Vec<String>, Vec<u32>, usize, usize) {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return (vec![], vec![], vec![], 0, 0) };
    let items = v.get("Items").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut people = vec![];
    let mut tags = vec![];
    let years = items.iter().filter_map(|i| i.get("ProductionYear")?.as_u64()).filter(|y| *y > 0).map(|y| y as u32).collect();
    for i in &items {
        // 同一支影片裡重複的只算一次
        let mut seen = std::collections::HashSet::new();
        for p in i.get("People").and_then(Value::as_array).into_iter().flatten() {
            if p.get("Type").and_then(Value::as_str).is_none_or(|t| t == "Actor") {
                if let Some(n) = p.get("Name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty()) {
                    if seen.insert(name_key(n)) {
                        people.push(n.to_string());
                    }
                }
            }
        }
        seen.clear();
        for t in i.get("Tags").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::trim).filter(|t| !t.is_empty()) {
            if seen.insert(name_key(t)) {
                tags.push(t.to_string());
            }
        }
    }
    let total = v.get("TotalRecordCount").and_then(Value::as_u64).unwrap_or(0) as usize;
    (people, tags, years, items.len(), total)
}

/// 標籤去掉前後空白、重複（只差大小寫、全形半形的也算重複）、依名稱排；年份去掉重複、新的在前
fn tidy_facets(tags: Vec<String>, mut years: Vec<u32>) -> (Vec<String>, Vec<u32>) {
    let mut tags: Vec<String> = tags.into_iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
    tags.sort_by_key(|t| (name_key(t), t.clone()));
    tags.dedup_by(|a, b| name_key(a) == name_key(b));
    years.sort_unstable_by(|a, b| b.cmp(a));
    years.dedup();
    (tags, years)
}

/// /Items/Filters 的回應：（標籤, 年份新的在前）
fn facets_of(json: &str) -> (Vec<String>, Vec<u32>) {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return (vec![], vec![]) };
    let tags: Vec<String> = v.get("Tags").and_then(Value::as_array).map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect()).unwrap_or_default();
    let years: Vec<u32> = v.get("Years").and_then(Value::as_array).map(|a| a.iter().filter_map(|y| y.as_u64().map(|y| y as u32)).collect()).unwrap_or_default();
    tidy_facets(tags, years)
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
        // 沒設年份時用首映日期的年（Jellyfin 的家庭影片頁面顯示的是首映日期）
        year: v.get("ProductionYear").and_then(Value::as_i64).map(|y| y as i32).or_else(|| premiere_year(&v)),
    })
}

/// 首映日期（2024-05-01T00:00:00Z）的年
fn premiere_year(v: &Value) -> Option<i32> {
    v.get("PremiereDate").and_then(Value::as_str).and_then(|d| d.get(..4)).and_then(|y| y.parse().ok())
}

/// 存好後重新讀回來，哪些欄位沒有改到（回傳欄位名稱；全部改到時空的）。
/// 標籤、人物不管順序與大小寫；標題空的時本來就不改
pub fn unsaved(want: &JfInfo, got: &JfInfo) -> Vec<&'static str> {
    let same_set = |a: &[String], b: &[String]| {
        let norm = |l: &[String]| {
            let mut v: Vec<String> = l.iter().map(|x| x.trim().to_lowercase()).filter(|x| !x.is_empty()).collect();
            v.sort();
            v.dedup();
            v
        };
        norm(a) == norm(b)
    };
    let mut out = vec![];
    if !want.title.trim().is_empty() && want.title.trim() != got.title.trim() {
        out.push(tr!("標題", "title"));
    }
    if want.overview.trim() != got.overview.trim() {
        out.push(tr!("說明", "description"));
    }
    if want.year != got.year {
        out.push(tr!("年份", "year"));
    }
    if !same_set(&want.people, &got.people) {
        out.push(tr!("人物", "people"));
    }
    if !same_set(&want.tags, &got.tags) {
        out.push(tr!("標籤", "tags"));
    }
    out
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
    set_year(o, x.year);
    serde_json::to_string(&v).ok()
}

/// 設年份：首映日期跟著換成這一年（月日不變）：Jellyfin 的家庭影片頁面顯示的是首映日期，只改年份會看不出來。
/// 年份清空時首映日期也清掉（對話框沒年份時顯示的就是首映日期的年，不清掉下次開啟又出現）
fn set_year(o: &mut serde_json::Map<String, Value>, year: Option<i32>) {
    o.insert("ProductionYear".into(), year.map_or(Value::Null, Value::from));
    match year {
        Some(y) => {
            let date = o.get("PremiereDate").and_then(Value::as_str).filter(|d| d.len() >= 4 && d.is_char_boundary(4)).map(|d| format!("{y:04}{}", &d[4..]));
            if let Some(d) = date {
                o.insert("PremiereDate".into(), Value::from(d));
            }
        }
        None => {
            o.insert("PremiereDate".into(), Value::Null);
        }
    }
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

/// 上次同步時 Jellyfin 我的最愛裡的檔名（小寫）：比對出新加的、移除的，本機的星號跟著改。存在設定資料夾的 jellyfin-favorites.json
static FAVS: std::sync::Mutex<Option<(Option<std::path::PathBuf>, std::collections::BTreeSet<String>)>> = std::sync::Mutex::new(None);

pub fn init_favorites(file: std::path::PathBuf) {
    let mut g = FAVS.lock().unwrap_or_else(|e| e.into_inner());
    if g.as_ref().is_some_and(|(f, _)| f.is_some()) {
        return;
    }
    let mut set: std::collections::BTreeSet<String> = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    set.extend(g.take().map(|(_, s)| s).unwrap_or_default());
    *g = Some((Some(file), set));
}

/// 和上次比：(新加進我的最愛的, 移除的)，並記下這次的
pub fn favorites_changed(now: &std::collections::HashSet<String>) -> (Vec<String>, Vec<String>) {
    let mut g = FAVS.lock().unwrap_or_else(|e| e.into_inner());
    let (file, prev) = g.get_or_insert_with(|| (None, Default::default()));
    let added: Vec<String> = now.iter().filter(|n| !prev.contains(*n)).cloned().collect();
    let removed: Vec<String> = prev.iter().filter(|n| !now.contains(*n)).cloned().collect();
    if !added.is_empty() || !removed.is_empty() {
        *prev = now.iter().cloned().collect();
        if let Some(f) = file.as_ref() {
            let _ = crate::paths::write_atomic(f, serde_json::to_string(prev).unwrap_or_default().as_bytes());
        }
    }
    (added, removed)
}

/// 本機改了星號、也改到 Jellyfin 之後記下（下次比對才不會又改回來）
pub fn favorite_set(name: &str, on: bool) {
    let mut g = FAVS.lock().unwrap_or_else(|e| e.into_inner());
    let (file, prev) = g.get_or_insert_with(|| (None, Default::default()));
    let name = name.to_lowercase();
    let changed = if on { prev.insert(name) } else { prev.remove(&name) };
    if changed {
        if let Some(f) = file.as_ref() {
            let _ = crate::paths::write_atomic(f, serde_json::to_string(prev).unwrap_or_default().as_bytes());
        }
    }
}

/// 傳好之後等 Jellyfin 掃到新檔案最多等多久（Jellyfin 忙、媒體庫很大時要比較久；等不到時影片資訊、封面就設不上去）
pub const WAIT_SCAN: Duration = Duration::from_secs(300);

/// 伺服器路徑 path 是不是 dir 本身或在它底下（/ 和 \\ 都算、不分大小寫）
fn path_under(path: &str, dir: &str) -> bool {
    let norm = |p: &str| p.replace('\\', "/").trim_end_matches('/').to_lowercase();
    let (path, dir) = (norm(path), norm(dir));
    path == dir || path.strip_prefix(&dir).is_some_and(|r| r.starts_with('/'))
}

/// 伺服器上的資料夾 remote 不在任何媒體庫裡時，回傳所有媒體庫的資料夾（讓使用者知道要傳到哪裡）；
/// 在媒體庫裡、或讀不懂媒體庫清單時 None
fn outside_libraries(libs: &str, remote: &str) -> Option<Vec<String>> {
    let Ok(Value::Array(libs)) = serde_json::from_str::<Value>(libs) else { return None };
    let locs: Vec<String> = libs.iter().filter_map(|l| l.get("Locations").and_then(Value::as_array)).flatten().filter_map(Value::as_str).map(str::to_string).collect();
    (!locs.is_empty() && !locs.iter().any(|l| path_under(remote, l))).then_some(locs)
}

/// 媒體庫清單裡在伺服器路徑 root 底下的資料夾，換成相對 root 的子資料夾（「family」「porn/cut」；就是 root 本身時 ""）。
/// 讀不懂清單時 None；都不在 root 底下時是空的（傳到哪裡 Jellyfin 都看不到）
fn subs_in_libraries(libs: &str, root: &str) -> Option<Vec<String>> {
    let Ok(Value::Array(libs)) = serde_json::from_str::<Value>(libs) else { return None };
    let norm = |p: &str| p.replace('\\', "/").trim_end_matches('/').to_string();
    let root = norm(root);
    let mut out: Vec<String> = vec![];
    for loc in libs.iter().filter_map(|l| l.get("Locations").and_then(Value::as_array)).flatten().filter_map(Value::as_str) {
        let loc = norm(loc);
        if loc.len() < root.len() || !loc.is_char_boundary(root.len()) || !loc[..root.len()].eq_ignore_ascii_case(&root) {
            continue;
        }
        let rest = &loc[root.len()..];
        if rest.is_empty() || rest.starts_with('/') {
            let sub = normalize_subfolder(rest);
            if !out.iter().any(|o| o.eq_ignore_ascii_case(&sub)) {
                out.push(sub);
            }
        }
    }
    out.sort_by_key(|s| s.to_lowercase());
    Some(out)
}

/// 子資料夾 sub 在 Jellyfin 看不看得到（scanned：library_subs 的結果）
#[derive(Debug, PartialEq)]
pub enum Seen {
    /// 在某個媒體庫裡
    Yes,
    /// 它本身不在，但底下有媒體庫的資料夾（相對 sub 的路徑，「cut」「uncensored」）
    Below(Vec<String>),
    /// 不在任何媒體庫裡、底下也沒有
    No,
}

/// 子資料夾 sub（「family/2024」；空的 = 最上層）是不是在 scanned 其中一個裡面（或就是它），不分大小寫
pub fn seen_by_jellyfin(scanned: &[String], sub: &str) -> Seen {
    let sub = normalize_subfolder(sub).to_lowercase();
    let under = |a: &str, b: &str| b.is_empty() || a == b || a.strip_prefix(b).is_some_and(|r| r.starts_with('/'));
    if scanned.iter().any(|s| under(&sub, &s.to_lowercase())) {
        return Seen::Yes;
    }
    let depth = sub_parts(&sub).count();
    let below: Vec<String> = scanned.iter().filter(|s| under(&s.to_lowercase(), &sub)).map(|s| s.split('/').skip(depth).collect::<Vec<_>>().join("/")).collect();
    if below.is_empty() {
        Seen::No
    } else {
        Seen::Below(below)
    }
}

/// 媒體庫（/Library/VirtualFolders）與資料夾（/Items 的 Folder）裡，伺服器路徑是 remote 本身或最近上層的那一個：（編號, 路徑）
fn closest_folder(libs: &str, folders: &str, remote: &str) -> Option<(String, String)> {
    let norm = |p: &str| p.replace('\\', "/").trim_end_matches('/').to_lowercase();
    let target = norm(remote);
    let mut found: Vec<(String, String)> = vec![];
    if let Ok(Value::Array(libs)) = serde_json::from_str::<Value>(libs) {
        for l in &libs {
            let Some(id) = l.get("ItemId").and_then(Value::as_str) else { continue };
            for loc in l.get("Locations").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
                found.push((id.to_string(), loc.to_string()));
            }
        }
    }
    if let Ok(v) = serde_json::from_str::<Value>(folders) {
        for f in v.get("Items").and_then(Value::as_array).into_iter().flatten() {
            if let (Some(id), Some(path)) = (f.get("Id").and_then(Value::as_str), f.get("Path").and_then(Value::as_str)) {
                found.push((id.to_string(), path.to_string()));
            }
        }
    }
    found
        .into_iter()
        .filter(|(_, p)| {
            let p = norm(p);
            target == p || target.strip_prefix(&p).is_some_and(|r| r.starts_with('/'))
        })
        .max_by_key(|(_, p)| norm(p).len())
}

/// 媒體庫 l 的資料夾有沒有包含 root（伺服器上的路徑；/ 和 \\ 都算、不分大小寫），或在 root 底下
fn covers(l: &Value, root: &str) -> bool {
    let norm = |p: &str| p.replace('\\', "/").trim_end_matches('/').to_lowercase();
    let root = norm(root);
    let under = |a: &str, b: &str| a == b || a.strip_prefix(b).is_some_and(|r| r.starts_with('/')) || b.is_empty();
    l.get("Locations").and_then(Value::as_array).is_some_and(|a| a.iter().filter_map(Value::as_str).map(norm).any(|loc| under(&root, &loc) || under(&loc, &root)))
}

/// 自動修正要開啟的媒體庫選項：即時監控、擷取章節圖片（含掃描時）、時間軸預覽縮圖（含掃描時）
const GOOD_OPTIONS: [&str; 5] =
    ["EnableRealtimeMonitor", "EnableChapterImageExtraction", "ExtractChapterImagesDuringLibraryScan", "EnableTrickplayImageExtraction", "ExtractTrickplayImagesDuringLibraryScan"];

/// 媒體庫選項開啟 GOOD_OPTIONS（其他照原樣）
fn with_good_options(opts: &Value) -> Value {
    let mut o = if opts.is_object() { opts.clone() } else { serde_json::json!({}) };
    if let Some(m) = o.as_object_mut() {
        for k in GOOD_OPTIONS {
            m.insert(k.into(), Value::Bool(true));
        }
    }
    o
}

/// 有沒有任何一個媒體庫開啟了這個選項
fn library_option_on(vf_json: &str, key: &str) -> bool {
    serde_json::from_str::<Value>(vf_json)
        .ok()
        .and_then(|v| v.as_array().map(|libs| libs.iter().any(|l| l.pointer(&format!("/LibraryOptions/{key}")).and_then(Value::as_bool) == Some(true))))
        .unwrap_or(false)
}

/// /Library/VirtualFolders 的回應裡，包含 server_root 的媒體庫設定對不對：類型是不是「家庭影片和照片」、
/// 即時監控、章節圖片、時間軸預覽縮圖有沒有開啟。server_root 不知道時（媒體庫還沒有影片）看不出來，None
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
                "：錄影會被當成電影或影集去網路上找資訊，可能配錯封面、名稱。按「自動修正」新建一個「家庭影片和照片」媒體庫",
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
        note: trf!("媒體庫「{names}」是家庭影片和照片，但{off}沒開啟：按「自動修正」開啟", "Library “{names}” is Home Videos and Photos, but {off} is off. Click “Fix it” to turn it on"),
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

/// /Items（fields=Path,MediaSources）的回應整理成索引：小寫檔名 → (位置, 大小)。
/// 位置是影片所在的媒體庫名稱加上在媒體庫裡的資料夾（「家庭影片/family/2024」）；libs 是 /Library/VirtualFolders 的回應。
/// 不在任何媒體庫位置底下的用它所在的資料夾名稱；沒有大小的略過（比不出是不是同一支）
pub fn index_from_items(libs: &str, items: &str) -> std::collections::HashMap<String, Vec<(String, u64)>> {
    let norm = |p: &str| p.replace('\\', "/").trim_end_matches('/').to_lowercase();
    let roots: Vec<(String, String)> = serde_json::from_str::<Value>(libs)
        .ok()
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .flat_map(|l| {
            let name = l.get("Name").and_then(Value::as_str).unwrap_or("").to_string();
            l.get("Locations").and_then(Value::as_array).cloned().unwrap_or_default().into_iter().filter_map(move |loc| loc.as_str().map(|s| (norm(s), name.clone())))
        })
        .collect();
    let mut out: std::collections::HashMap<String, Vec<(String, u64)>> = std::collections::HashMap::new();
    let Ok(v) = serde_json::from_str::<Value>(items) else { return out };
    for it in v.get("Items").and_then(Value::as_array).into_iter().flatten() {
        let Some(path) = it.get("Path").and_then(Value::as_str) else { continue };
        let Some(size) = it.pointer("/MediaSources/0/Size").and_then(Value::as_u64).filter(|s| *s > 0) else { continue };
        let original = path.replace('\\', "/");
        let (dir, file) = original.rsplit_once('/').unwrap_or(("", original.as_str()));
        let low = dir.to_lowercase();
        let place = match roots.iter().filter(|(r, _)| low == *r || low.starts_with(&format!("{r}/"))).max_by_key(|(r, _)| r.len()) {
            Some((r, name)) => std::iter::once(name.as_str()).chain(dir[r.len()..].split('/').filter(|s| !s.is_empty())).collect::<Vec<_>>().join("/"),
            None => dir.rsplit('/').next().unwrap_or("").to_string(),
        };
        out.entry(file.to_lowercase()).or_default().push((place, size));
    }
    out
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
            // 兩邊的大小不一樣了：記下來，「已在媒體庫」、跟著重新命名、刪除照樣認得
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
    let mut g = LINKS.lock().unwrap_or_else(|e| e.into_inner());
    if g.as_ref().is_some_and(|(f, _)| f.is_some()) {
        return;
    }
    let mut list: Vec<(String, u64, u64)> = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    list.extend(g.take().map(|(_, l)| l).unwrap_or_default());
    *g = Some((Some(file), list));
}

fn with_links<T>(f: impl FnOnce(&mut Vec<(String, u64, u64)>) -> (T, bool)) -> T {
    let mut g = LINKS.lock().unwrap_or_else(|e| e.into_inner());
    let (file, list) = g.get_or_insert_with(|| (None, vec![]));
    let (out, changed) = f(list);
    if changed {
        if let Some(file) = file.as_ref() {
            if let Err(e) = crate::paths::write_atomic(file, serde_json::to_string(list).unwrap_or_default().as_bytes()) {
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

/// 本機重新命名後，記下的對照也換成新名稱
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
        // 影片已經不在（改了名、刪掉了）：不要傳 0 個檔案之後白等 Jellyfin 找一支不存在的影片
        if files.is_empty() {
            crate::warn!("[傳到資料夾] 找不到 {}", file.display());
            return Some(Err(trf!(
                "找不到「{name}」，可能已經重新命名或刪掉了：請在清單上重新找到這支再傳",
                "Couldn't find “{name}”; it may have been renamed or deleted. Find it in the list again and resend"
            )));
        }
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
    // 新的檔案等它掃到（最多 5 分鐘）；已經有的直接設定
    Some(match jf.refresh_items(&name, Some(&lib.join(library_name(file)))) {
        Ok(ids) => {
            let what = if ids.is_empty() { tr!("，已請 Jellyfin 掃描", ", Jellyfin asked to scan") } else { tr!("，已請 Jellyfin 重新讀取", ", Jellyfin asked to reload it") };
            let more = jf.after_upload(&name, extras, ids, WAIT_SCAN, Some(&lib.join(library_name(file))));
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
        .then(|| trf!("Jellyfin 的資料夾只剩 {}，這支要 {}。先清出空間再傳", "The Jellyfin folder only has {} free and this needs {}. Free up some space first", fmt(free), fmt(need)))
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

/// 檔案本來就放在媒體庫（最上層或底下任何一層，不含 NAS 的系統資料夾）裡：重新命名、刪除就是改到媒體庫那一份
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
/// 找最上層和底下每一層；檔案本來就在媒體庫裡時是同一份，回傳空的；file 不用還在（重新命名、刪除之後也查得到）
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

/// 重新命名後：媒體庫裡那一份（連同字幕）也換成新名稱；回傳改好的檔案（新名稱）。重新命名不會改內容：用新檔案的大小比對，別的資料夾裡剛好同名的不動
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
            return Err(trf!("Jellyfin 的資料夾裡已經有「{}」", "The Jellyfin folder already has “{}”", to.file_name().unwrap_or_default().to_string_lossy()));
        }
        std::fs::rename(&f, &to).map_err(|e| crate::error::plain(&e))?;
        crate::info!("[傳到資料夾] 跟著重新命名：{} → {}", f.display(), to.display());
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

/// 伺服器對「who is JellyfinServer?」的回覆：{"Address":"http://192.168.1.10:8096","Name":"NAS",…}。
/// 網址一律連到「回覆的那台」（回覆裡的主機和來源不同時只用它的 http/https 和埠：
/// 不會被假的回覆帶到別台主機去送 API 金鑰；Docker 回報容器內的 IP 時也連得到）。名稱後面加上 IP 讓使用者認得是哪台
fn parse_discovery(json: &str, from: std::net::SocketAddr) -> Option<(String, String)> {
    let v: Value = serde_json::from_str(json).ok()?;
    v.get("Id")?;
    let ip = from.ip();
    let host = if ip.is_ipv6() { format!("[{ip}]") } else { ip.to_string() };
    let name = format!("{}（{ip}）", v.get("Name").and_then(Value::as_str).unwrap_or("Jellyfin"));
    let url = match v.get("Address").and_then(Value::as_str).and_then(normalize_url) {
        Some(addr) => {
            let (scheme, rest) = addr.split_once("://").unwrap_or(("http", &addr));
            let authority = rest.split('/').next().unwrap_or("");
            // [IPv6]:埠 或 主機:埠
            let (h, port) = match authority.strip_prefix('[') {
                Some(r) => r.split_once(']').map_or((r, ""), |(h, p)| (h, p.trim_start_matches(':'))),
                None => authority.rsplit_once(':').unwrap_or((authority, "")),
            };
            let port = Some(port).filter(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
            let path = &rest[authority.len()..];
            // 主機名稱（https 的憑證是發給名稱的）：查得到、就是回覆的那台才照用
            let same = match h.parse::<std::net::IpAddr>() {
                Ok(a) => a == ip,
                Err(_) => std::net::ToSocketAddrs::to_socket_addrs(&(h, 0)).is_ok_and(|mut it| it.any(|a| a.ip() == ip)),
            };
            if same {
                addr.clone()
            } else {
                crate::info!("[Jellyfin] 探索回覆的網址是 {addr}，改連回覆的那台 {ip}");
                format!("{scheme}://{host}:{}{path}", port.unwrap_or("8096"))
            }
        }
        None => format!("http://{host}:8096"),
    };
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

/// 區域網路的伺服器：直接連線（不經過 Proxy），等太久就放棄。整個程式共用一個（連線會留著重複用，
/// 一頁幾十張縮圖不用每張都重新連線、重新做 TLS 交握）
fn agent() -> ureq::Agent {
    static A: std::sync::LazyLock<ureq::Agent> = std::sync::LazyLock::new(|| agent_with(Duration::from_secs(15)));
    A.clone()
}

/// 下載整支影片用：每次讀等久一點（NAS 從硬碟休眠醒來要幾秒）
fn agent_long() -> ureq::Agent {
    static A: std::sync::LazyLock<ureq::Agent> = std::sync::LazyLock::new(|| agent_with(Duration::from_secs(60)));
    A.clone()
}

/// 本機轉送串流用（jf_proxy.rs）
pub(crate) fn stream_agent() -> ureq::Agent {
    agent_long()
}

/// 不跟著轉址走：ureq 轉址時只移除 Authorization，X-Emby-Token（API 金鑰）會跟著送到別的主機。
/// 寫入也設逾時（上傳封面、字幕時伺服器不收資料不會一直等）
fn agent_with(read: Duration) -> ureq::Agent {
    let b = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(8)).timeout_read(read).timeout_write(Duration::from_secs(60)).redirects(0);
    #[cfg(windows)]
    let b = match native_tls::TlsConnector::new() {
        Ok(c) => b.tls_connector(std::sync::Arc::new(c)),
        Err(_) => b,
    };
    b.build()
}

/// 連線在送出後被關掉或重設（共用的連線閒置太久被伺服器或中間的設備斷掉）：ureq 只對 GET 這類自動重送，
/// POST（存影片資訊、封面）不會。這種情況伺服器沒收到完整的要求，換一條新連線再送一次；逾時不重送
fn stale(e: &ureq::Error) -> bool {
    let ureq::Error::Transport(t) = e else { return false };
    if t.kind() != ureq::ErrorKind::Io {
        return false;
    }
    match std::error::Error::source(t).and_then(|s| s.downcast_ref::<std::io::Error>()) {
        Some(io) => matches!(io.kind(), std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::UnexpectedEof),
        None => t.to_string().to_ascii_lowercase().contains("closed"),
    }
}

/// 送出（body：POST 的內容，None = 沒有內容）；連線斷掉（見 stale）時再送一次
fn resend(req: ureq::Request, body: Option<&str>) -> Result<ureq::Response, String> {
    let first = match body {
        Some(b) => req.clone().send_string(b),
        None => req.clone().call(),
    };
    let r = match first {
        Err(e) if stale(&e) => {
            crate::info!("[Jellyfin] 連線斷掉了，換一條重送：{} {}（{e}）", req.method(), req.url());
            match body {
                Some(b) => req.send_string(b),
                None => req.call(),
            }
        }
        r => r,
    };
    r.map_err(|e| describe(&e))
}

fn describe(e: &ureq::Error) -> String {
    // 連線層的錯誤畫面上只寫「連不到」，原因記在記錄檔（逾時、被拒絕、被重設…）
    if let ureq::Error::Transport(t) = e {
        crate::warn!("[Jellyfin] 連線錯誤：{t}");
    }
    match e {
        ureq::Error::Status(401 | 403, _) => tr!("API 金鑰不對（Jellyfin 控制台 → API 金鑰）", "The API key is wrong (Jellyfin Dashboard → API Keys)").into(),
        ureq::Error::Status(404, _) => tr!("網址不對，找不到 Jellyfin（預設是 http://伺服器:8096）", "Wrong address; Jellyfin wasn't found there (usually http://server:8096)").into(),
        ureq::Error::Status(300..=399, r) => {
            let to = r.header("location").unwrap_or("").to_string();
            trf!("伺服器要求改用另一個網址（{to}），請在設定裡把 Jellyfin 網址改成它", "The server asks to use another address ({to}); change the Jellyfin address in Settings to it")
        }
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
    fn a_post_on_a_dropped_connection_is_sent_again() {
        // 第一次：伺服器讀完要求就斷線、不回應；第二次正常回應
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            let mut got = vec![];
            for answer in [false, true] {
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
                if answer {
                    write!(s, "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n").unwrap();
                } else {
                    let _ = s.shutdown(std::net::Shutdown::Both);
                }
            }
            got
        });
        let jf = Jellyfin::new(&url, "k").unwrap();
        jf.call_body(agent().post(&format!("{url}/Items/x")), "{}").unwrap();
        assert_eq!(h.join().unwrap(), vec!["POST /Items/x HTTP/1.1", "POST /Items/x HTTP/1.1"]);
        // 逾時、連不上不重送
        let refused = ureq::get("http://127.0.0.1:1/").call().unwrap_err();
        assert!(!stale(&refused));
    }

    #[test]
    fn item_people_come_with_their_ids() {
        let body = r#"{"Id":"v","People":[{"Name":"つかもと友希","Type":"Actor","Id":"p1"},{"Name":"導演","Type":"Director","Id":"d1"},{"Name":" 新人 "},{"Name":"","Id":"x"}]}"#;
        assert_eq!(people_ids_of(body), Some(vec![(Some("p1".into()), "つかもと友希".into()), (None, "新人".into())]));
        assert_eq!(people_ids_of(r#"{"Id":"v"}"#), Some(vec![]));
        assert_eq!(people_ids_of("not json"), None);
    }

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
        // 先用名稱搜尋（只回傳幾筆），不用列出全部影片
        assert!(req[0].starts_with("GET /Items?searchTerm=Rec_1&recursive=true&") && req[0].contains("fields=Path"), "{req:?}");
        assert!(req[1].starts_with("POST /Items/a1/Refresh?metadataRefreshMode=Default&imageRefreshMode=FullRefresh&replaceAllMetadata=false&replaceAllImages=true"), "{req:?}");
        assert!(req[2].starts_with("POST /Items/c3/Refresh?"), "{req:?}");
        // Jellyfin 還沒有這支：請它掃描媒體庫
        let (url, h) = serve_seq(vec![("200 OK", r#"{"Items":[]}"#.to_string()), ("200 OK", r#"{"Items":[]}"#.to_string()), ("204 No Content", String::new())]);
        assert_eq!(Jellyfin::new(&url, "k").unwrap().refresh_file("new.mp4").unwrap(), 0);
        let req = h.join().unwrap();
        // 名稱搜尋找不到：比對全部影片的檔名，還是沒有才請它掃描
        assert!(req[0].contains("searchTerm=new&") && req[1].starts_with("GET /Items?recursive=true&"), "{req:?}");
        assert!(req[2].starts_with("POST /Library/Refresh "), "{req:?}");
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
        let (url, h) = serve_seq(vec![("200 OK", r#"{"Items":[]}"#.to_string()), ("200 OK", r#"{"Items":[]}"#.to_string()), ("204 No Content", String::new()), ("200 OK", found)]);
        let jf = Jellyfin::new(&url, "k").unwrap();
        let msg = send_file(Some(&jf), &copy_to, &video, &Extras::default()).unwrap().unwrap();
        assert!(msg.contains("✓"), "{msg}");
        let req = h.join().unwrap();
        assert!(req[2].starts_with("POST /Library/Refresh "), "{req:?}");
        // 等它掃到：用名稱搜尋，不用每次列出全部影片
        assert!(req[3].starts_with("GET /Items?searchTerm=Rec_1_cut&"), "{req:?}");
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
        assert_eq!(jf.url, "http://nas:8096");
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

        // 重新命名只動同大小的那一份，baseball 裡的別支不動
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
        let libs = r#"[{"Name":"錄影","ItemId":"L1","Locations":["/srv/video"]}]"#.to_string();
        let folders = r#"{"Items":[{"Id":"F1","Path":"/srv/video/family"},{"Id":"F2","Path":"/srv/video/family2"}]}"#.to_string();
        // 直接請它重新整理 family 那個資料夾（馬上掃，不等即時監控的延遲）
        let (url, h) = serve_seq(vec![("200 OK", items.clone()), ("200 OK", libs.clone()), ("200 OK", folders), ("204 No Content", String::new())]);
        let jf = Jellyfin::new(&url, "k").unwrap().with_root(&lib.display().to_string());
        assert!(jf.scan(&lib.join("family/new.mp4")).unwrap());
        let got = h.join().unwrap();
        assert!(got[0].starts_with("GET /Items?"), "{got:?}");
        assert!(got[1].starts_with("GET /Library/VirtualFolders"), "{got:?}");
        assert!(got[3].starts_with("POST /Items/F1/Refresh?Recursive=true"), "{got:?}");
        // 重新整理失敗（舊版）：改成通知它有新檔案
        let (url, h) = serve_seq(vec![("200 OK", items), ("200 OK", libs), ("200 OK", r#"{"Items":[]}"#.to_string()), ("404 Not Found", String::new()), ("204 No Content", String::new())]);
        let jf = Jellyfin::new(&url, "k").unwrap().with_root(&lib.display().to_string());
        assert!(jf.scan(&lib.join("family/new.mp4")).unwrap());
        let got = h.join().unwrap();
        assert!(got[3].starts_with("POST /Items/L1/Refresh"), "{got:?}");
        assert!(got[4].starts_with("POST /Library/Media/Updated"), "{got:?}");
        // 對不上（媒體庫還沒有影片）：掃描所有媒體庫
        let (url, h) = serve_seq(vec![("200 OK", r#"{"Items":[]}"#.to_string()), ("204 No Content", String::new())]);
        let jf = Jellyfin::new(&url, "k").unwrap().with_root(&dir.path().join("empty").display().to_string());
        assert!(!jf.scan(&dir.path().join("empty/a.mp4")).unwrap());
        assert!(h.join().unwrap()[1].starts_with("POST /Library/Refresh"));
    }

    #[test]
    fn folders_outside_every_library_are_reported() {
        let libs = r#"[{"Name":"X","ItemId":"L1","Locations":["/video/X Pron"]},{"Name":"錄影","ItemId":"L2","Locations":["/video/family/"]}]"#;
        // 放在 /video 最上層：不在任何媒體庫裡
        assert_eq!(outside_libraries(libs, "/video"), Some(vec!["/video/X Pron".to_string(), "/video/family/".to_string()]));
        assert_eq!(outside_libraries(libs, "/video/familyX"), Some(vec!["/video/X Pron".to_string(), "/video/family/".to_string()]));
        // 在媒體庫裡（本身或底下，大小寫、斜線不同也算）
        assert_eq!(outside_libraries(libs, "/video/family"), None);
        assert_eq!(outside_libraries(libs, "/VIDEO/x pron/2024"), None);
        assert_eq!(outside_libraries(r#"[{"Locations":["D:\\Media"]}]"#, "d:\\media\\sub"), None);
        // 讀不懂或沒有媒體庫：不下結論
        assert_eq!(outside_libraries("", "/video"), None);
        assert_eq!(outside_libraries("[]", "/video"), None);
    }

    #[test]
    fn sending_a_missing_file_fails_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let (src, lib) = (dir.path().join("rec"), dir.path().join("lib"));
        std::fs::create_dir_all(&src).unwrap();
        let r = send_file(None, &lib.display().to_string(), &src.join("Rec_2026-10-23_15-32-38.mp4"), &Extras::default()).unwrap();
        assert!(r.unwrap_err().contains("找不到"));
        assert!(!lib.exists());
    }

    #[test]
    fn library_folders_become_subfolders_jellyfin_can_see() {
        let libs = r#"[{"Locations":["/video/porn/uncensored","/video/piano"]},{"Locations":["/config/data/collections","/video/Family/"]},{"Locations":["/videos2"]}]"#;
        let subs = subs_in_libraries(libs, "/video").unwrap();
        assert_eq!(subs, vec!["Family", "piano", "porn/uncensored"]);
        assert_eq!(subs_in_libraries(r#"[{"Locations":["/video"]}]"#, "/video/").unwrap(), vec![""]);
        assert_eq!(subs_in_libraries("oops", "/video"), None);

        assert_eq!(seen_by_jellyfin(&subs, "family/2024"), Seen::Yes);
        assert_eq!(seen_by_jellyfin(&subs, "Piano"), Seen::Yes);
        assert_eq!(seen_by_jellyfin(&subs, ""), Seen::Below(vec!["Family".into(), "piano".into(), "porn/uncensored".into()]));
        assert_eq!(seen_by_jellyfin(&subs, "porn"), Seen::Below(vec!["uncensored".into()]));
        assert_eq!(seen_by_jellyfin(&subs, "pianos"), Seen::No);
        assert_eq!(seen_by_jellyfin(&subs, "other"), Seen::No);
        // 整個資料夾都掃
        assert_eq!(seen_by_jellyfin(&[String::new()], "other"), Seen::Yes);
    }

    #[test]
    fn closest_folder_is_the_folder_itself_or_its_nearest_parent() {
        let libs = r#"[{"Name":"錄影","ItemId":"L1","Locations":["/volume1/video/"]},{"Name":"電影","ItemId":"L2","Locations":["/volume1/movies"]},{"Name":"壞的"}]"#;
        let folders = r#"{"Items":[{"Id":"F1","Path":"/volume1/video/family"},{"Id":"F2","Path":"/volume1/video/family/2024"},{"Id":"F3","Path":"/volume1/video/fam"}]}"#;
        let id = |remote: &str| closest_folder(libs, folders, remote).map(|(id, _)| id);
        assert_eq!(id("/volume1/video/family/2024").as_deref(), Some("F2"));
        // 新的子資料夾 Jellyfin 還沒有：上一層
        assert_eq!(id("/volume1/video/family/2025").as_deref(), Some("F1"));
        // 媒體庫最上層（名稱只是開頭一樣的不算）
        assert_eq!(id("/volume1/video").as_deref(), Some("L1"));
        assert_eq!(id("/volume1/video/familyX").as_deref(), Some("L1"));
        // Windows 伺服器、大小寫
        assert_eq!(closest_folder(r#"[{"ItemId":"W","Locations":["D:\\Media"]}]"#, "", "d:\\media\\other").map(|(id, _)| id).as_deref(), Some("W"));
        // 不在任何媒體庫
        assert_eq!(id("/volume1/other"), None);
        assert_eq!(closest_folder("", "", "/x"), None);
    }

    #[test]
    fn waiting_for_a_new_file_looks_only_in_its_folder() {
        let none = r#"{"Items":[{"Id":"o","Path":"/v/family/old.mp4"}]}"#.to_string();
        let found = r#"{"Items":[{"Id":"n1","Path":"/v/family/new.mp4"}]}"#.to_string();
        let (url, h) = serve_seq(vec![("200 OK", none), ("200 OK", found)]);
        let jf = Jellyfin::new(&url, "k").unwrap();
        let ticks = std::cell::Cell::new(0);
        assert_eq!(jf.wait_for("new.mp4", Duration::from_secs(30), Some("F1"), |_| ticks.set(ticks.get() + 1)).unwrap().as_deref(), Some("n1"));
        assert_eq!(ticks.get(), 1);
        let got = h.join().unwrap();
        assert!(got.iter().all(|g| g.contains("parentId=F1")), "{got:?}");
        // 時間到還沒有：None
        let (url, h) = serve_seq(vec![("200 OK", r#"{"Items":[]}"#.to_string())]);
        let jf = Jellyfin::new(&url, "k").unwrap();
        assert_eq!(jf.wait_for("new.mp4", Duration::ZERO, None, |_| {}).unwrap(), None);
        assert!(!h.join().unwrap()[0].contains("parentId"));
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
        assert_eq!(found, vec![("家裡的NAS（127.0.0.1）".to_string(), "http://127.0.0.1:8096".to_string())]);
        let from = std::net::SocketAddr::from(([10, 0, 0, 5], 7359));
        assert_eq!(parse_discovery(r#"{"Id":"x","Name":"N"}"#, from), Some(("N（10.0.0.5）".into(), "http://10.0.0.5:8096".into())));
        // 回覆指向別台主機（假的回覆、Docker 容器內的 IP）：連回覆的那台，保留 https 和埠
        assert_eq!(parse_discovery(r#"{"Id":"x","Name":"N","Address":"https://evil.example:8920/jf"}"#, from).unwrap().1, "https://10.0.0.5:8920/jf");
        assert_eq!(parse_discovery(r#"{"Id":"x","Name":"N","Address":"http://10.0.0.5:8097"}"#, from).unwrap().1, "http://10.0.0.5:8097");
        assert_eq!(parse_discovery(r#"{"Id":"x","Name":"N","Address":"http://172.17.0.2"}"#, from).unwrap().1, "http://10.0.0.5:8096");
        let local = std::net::SocketAddr::from(([127, 0, 0, 1], 7359));
        assert_eq!(parse_discovery(r#"{"Id":"x","Name":"N","Address":"https://localhost:8920"}"#, local).unwrap().1, "https://localhost:8920");
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
        // 重新命名跟著走
        let renamed = local.join("j3新名.mp4");
        std::fs::rename(&video, &renamed).unwrap();
        assert_eq!(rename_copies(&copy_to, &video, &renamed).unwrap(), vec![lib.join("family/j3新名.mp4")]);
        assert!(same_copy("j3新名.mp4", 10, 3));
        // 換畫面（旋轉）：整支重傳；Jellyfin 還沒有這支時封面也整支重傳
        let (url, h) = serve_seq(vec![
            ("200 OK", r#"{"Items":[]}"#.to_string()),
            ("200 OK", r#"{"Items":[]}"#.to_string()),
            ("200 OK", r#"{"Items":[]}"#.to_string()),
            ("200 OK", r#"{"Items":[]}"#.to_string()),
            ("204 No Content", String::new()),
        ]);
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

        // 自動修正：已經是家庭影片 → 開啟選項
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
        // 移除「只給大人看」：清掉 R；別的分級不動
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
        // 給 FFmpeg 的網址不帶金鑰（本機轉送），找回來的是原本的伺服器網址
        assert!(!url.contains("secret"), "{url}");
        assert_eq!(crate::jf_proxy::resolve(&url), Some(("http://nas:8096/Videos/v1/stream?static=true".into(), "secret".into())));
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
        jf.items("u", Some("p1"), "", 0, 60, JfSort::Name, JfFilter::Unplayed, &[]).unwrap();
        let got = h.join().unwrap();
        assert!(got[0].contains("filters=IsUnplayed") && got[0].contains("recursive=true") && got[0].contains("parentId=p1") && got[0].contains("sortBy=SortName"), "{got:?}");
    }

    #[test]
    fn facet_browse_searches_below_the_folder() {
        // 在資料夾裡：只找那個資料夾（含子資料夾）
        let (url, h) = serve_seq(vec![("200 OK", r#"{"Items":[],"TotalRecordCount":0}"#.to_string())]);
        let jf = Jellyfin::new(&url, "k").unwrap();
        jf.items("u", Some("p1"), "", 0, 60, JfSort::Name, JfFilter::All, &[JfFacet::Person("pp".into(), "媽媽".into()), JfFacet::Person("pq".into(), "小明".into()), JfFacet::Year(2024)]).unwrap();
        let got = h.join().unwrap();
        assert!(got[0].contains("personIds=pp%2Cpq") && got[0].contains("years=2024") && got[0].contains("recursive=true") && got[0].contains("parentId=p1"), "{got:?}");
        // 最上層：整個伺服器
        let (url, h) = serve_seq(vec![("200 OK", r#"{"Items":[],"TotalRecordCount":0}"#.to_string())]);
        let jf = Jellyfin::new(&url, "k").unwrap();
        jf.items("u", None, "", 0, 60, JfSort::Name, JfFilter::All, &[JfFacet::Year(2024)]).unwrap();
        let got = h.join().unwrap();
        assert!(got[0].contains("years=2024") && !got[0].contains("parentId"), "{got:?}");
        // 「沒有人物」也只看那個資料夾
        let (url, h) = serve_seq(vec![("200 OK", r#"{"Items":[{"Id":"a","Name":"a","Type":"Video"}],"TotalRecordCount":1}"#.to_string())]);
        let jf = Jellyfin::new(&url, "k").unwrap();
        let (items, n) = jf.items("u", Some("p1"), "", 0, 60, JfSort::Name, JfFilter::All, &[JfFacet::NoPeople]).unwrap();
        let got = h.join().unwrap();
        assert!(got[0].contains("parentId=p1") && got[0].contains("recursive=true"), "{got:?}");
        assert_eq!((items.len(), n), (1, 1));
    }

    #[test]
    fn usage_counts_only_the_folder() {
        let body = r#"{"Items":[{"Id":"a","People":[],"Tags":[]},{"Id":"b","People":[{"Name":"小美"}],"Tags":["旅行"]}],"TotalRecordCount":2}"#;
        let (url, h) = serve_seq(vec![("200 OK", body.to_string())]);
        let jf = Jellyfin::new(&url, "k").unwrap();
        let u = jf.usage_in("u", Some("p1")).unwrap();
        let got = h.join().unwrap();
        assert!(got[0].contains("parentId=p1") && got[0].contains("recursive=true"), "{got:?}");
        assert_eq!((u.no_people, u.no_tags), (1, 1));
        // 整個伺服器：不帶資料夾
        let (url, h) = serve_seq(vec![("200 OK", body.to_string())]);
        let jf = Jellyfin::new(&url, "k").unwrap();
        jf.usage("u").unwrap();
        assert!(!h.join().unwrap()[0].contains("parentId"));
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
        // 改年份：首映日期跟著換年；沒有年份時用首映日期的年
        let dated = r#"{"Id":"x","Name":"生日","PremiereDate":"2026-05-01T10:00:00.0000000Z"}"#;
        assert_eq!(info_of(dated).unwrap().year, Some(2026));
        let v: Value = serde_json::from_str(&with_info(dated, &JfInfo { year: Some(2019), ..Default::default() }).unwrap()).unwrap();
        assert_eq!((v["ProductionYear"].as_i64(), v["PremiereDate"].as_str()), (Some(2019), Some("2019-05-01T10:00:00.0000000Z")));
        assert_eq!(v["Name"], "生日");
        // 清掉年份：首映日期也清掉，下次開啟不會又出現
        let v: Value = serde_json::from_str(&with_info(dated, &JfInfo::default()).unwrap()).unwrap();
        assert!(v["ProductionYear"].is_null() && v["PremiereDate"].is_null());
        assert_eq!(info_of(&v.to_string()).unwrap().year, None);
    }

    #[test]
    fn collects_tags_and_years_from_items() {
        let page = r#"{"Items":[{"Id":"a","Tags":["生日","旅行"],"ProductionYear":2024},{"Id":"b","Tags":["生日 "," "]},{"Id":"c","ProductionYear":2019}],"TotalRecordCount":7}"#;
        let (tags, years, got, total) = tags_years_of_items(page);
        assert_eq!((got, total), (3, 7));
        assert_eq!(tidy_facets(tags, years), (vec!["旅行".to_string(), "生日".to_string()], vec![2024, 2019]));
    }

    #[test]
    fn usage_counts_merge_spellings() {
        let json = r#"{"TotalRecordCount":3,"Items":[
            {"People":[{"Name":"小美","Type":"Actor"},{"Name":"導演","Type":"Director"}],"Tags":["無碼","ＶＲ"]},
            {"People":[{"Name":" 小美 ","Type":"Actor"},{"Name":"阿明"}],"Tags":["無碼 ","vr","vr"],"ProductionYear":2024},
            {"People":[],"Tags":["旅行"],"ProductionYear":2024}]}"#;
        let (people, tags, years, got, total) = people_tags_of_items(json);
        assert_eq!((got, total), (3, 3));
        assert_eq!(tally_years(years), vec![(2024, 2)]);
        assert_eq!(tally(people), vec![("小美".to_string(), 2), ("阿明".to_string(), 1)]);
        // 次數一樣時依名稱排：vr（兩種寫法各一次）、無碼（「無碼 」去掉空白後一樣），最後是只用一次的旅行
        let t = tally(tags);
        assert_eq!(t.len(), 3);
        assert_eq!((name_key(&t[0].0), t[0].1), ("vr".to_string(), 2));
        assert_eq!(t[1], ("無碼".to_string(), 2));
        assert_eq!(t[2], ("旅行".to_string(), 1));
        assert_eq!(name_key("　ＡＢＣ　"), "abc");
    }

    #[test]
    fn batch_adds_and_removes_only_what_changed() {
        let item = r#"{"Id":"a","Name":"x","Tags":["無碼","18+","素人"],"People":[{"Name":"小美","Type":"Actor","Id":"p1"},{"Name":"導演","Type":"Director"}],"ProductionYear":2023,"PremiereDate":"2023-05-01T00:00:00.0000000Z"}"#;
        let e = BatchEdit {
            add_people: vec!["小明".into(), " 小美 ".into()],
            remove_people: vec!["導演".into()],
            add_tags: vec!["ＶＲ".into(), "無碼 ".into()],
            remove_tags: vec!["18+".into()],
            year: Some(Some(2024)),
        };
        let v: Value = serde_json::from_str(&with_batch(item, &e).unwrap()).unwrap();
        // 已經有的（寫法只差空白、全形半形）不再加；移除的只移除那一個；導演不是演員，移除人物時不動
        assert_eq!(v["Tags"], serde_json::json!(["無碼", "素人", "ＶＲ"]));
        assert_eq!(v["People"], serde_json::json!([{"Name":"小美","Type":"Actor","Id":"p1"},{"Name":"導演","Type":"Director"},{"Name":"小明","Type":"Actor"}]));
        assert_eq!(v["ProductionYear"], 2024);
        assert_eq!(v["PremiereDate"], "2024-05-01T00:00:00.0000000Z");
        // 年份不改時不動
        let v: Value = serde_json::from_str(&with_batch(item, &BatchEdit { add_tags: vec!["旅行".into()], ..Default::default() }).unwrap()).unwrap();
        assert_eq!(v["ProductionYear"], 2023);
        assert!(BatchEdit::default().is_empty());
    }

    #[test]
    fn local_path_maps_server_paths_back() {
        assert_eq!(local_path("/volume1/video", "Y:\\", "/volume1/video/family/2024/a.mp4"), Some(std::path::PathBuf::from("Y:\\").join("family").join("2024").join("a.mp4")));
        assert_eq!(local_path("/volume1/video/", "/mnt/v", "/Volume1/Video/a.mp4"), Some(std::path::PathBuf::from("/mnt/v/a.mp4")));
        assert_eq!(local_path("/volume1/video", "/mnt/v", "/volume1/videos/a.mp4"), None);
        assert_eq!(local_path("/volume1/video", "", "/volume1/video/a.mp4"), None);
        assert_eq!(local_path("/volume1/video", "/mnt/v", "/volume1/video/../../etc/x"), None);
        assert_eq!(local_path("/volume1/video", "Y:\\", "/volume1/video/C:/Windows/x.mp4"), None);
        assert_eq!(subtitle_lang("zh"), "zh-TW");
        assert_eq!(subtitle_lang("auto"), "und");
    }

    #[test]
    fn lists_are_hidden_from_views_and_entries_parsed() {
        let views = r#"{"Items":[{"Id":"a","Name":"家庭影片","Type":"CollectionFolder","IsFolder":true,"CollectionType":"homevideos"},
            {"Id":"b","Name":"合集","Type":"CollectionFolder","IsFolder":true,"CollectionType":"boxsets"},
            {"Id":"c","Name":"播放清單","Type":"ManualPlaylistsFolder","IsFolder":true,"CollectionType":"playlists"}]}"#;
        assert_eq!(list_views(views), ["b", "c"]);
        let pl = r#"{"Items":[{"Id":"v1","Type":"Video","Name":"x","PlaylistItemId":"e9"},{"Id":"p","Type":"Playlist","IsFolder":true,"Name":"週末","ChildCount":3}]}"#;
        let (items, _) = parse_items(pl);
        assert_eq!(items[0].entry, "e9");
        assert_eq!((items[1].kind, items[1].children), (JfKind::Folder, 3));
    }

    #[test]
    fn lacking_keeps_videos_without_people_or_tags() {
        let page = r#"{"TotalRecordCount":9,"Items":[
            {"Id":"a","Type":"Video","Tags":["生日"],"People":[]},
            {"Id":"b","Type":"Video","Tags":[],"People":[{"Name":"導演","Type":"Director"}]},
            {"Id":"c","Type":"Video","Tags":[" "],"People":[{"Name":"小美","Type":"Actor"}]},
            {"Id":"d","Type":"Video","MediaSources":[{"Size":1234}],"ProductionYear":2021,"People":[{"Name":"小美"}],"Tags":["旅行"]}]}"#;
        let mut keep = vec![];
        assert_eq!(lacking_in(page, &[NameKind::Person], &mut keep), (4, 9));
        // 導演不算人物
        assert_eq!(keep.iter().map(|i| i["Id"].as_str().unwrap()).collect::<Vec<_>>(), ["a", "b"]);
        keep.clear();
        lacking_in(page, &[NameKind::Person, NameKind::Tag], &mut keep);
        assert_eq!(keep.iter().map(|i| i["Id"].as_str().unwrap()).collect::<Vec<_>>(), ["b"]);
        assert_eq!(lacking_counts(page), (2, 2));
        // 清單上的人物、標籤、年份、大小
        let d = &parse_items(page).0[3];
        assert_eq!((d.people.clone(), d.tags.clone(), d.year, d.size), (vec!["小美".to_string()], vec!["旅行".to_string()], Some(2021), 1234));
        // 「沒有人物」不是伺服器的查詢參數
        assert!(facet_query(&[JfFacet::NoPeople, JfFacet::NoTags]).is_empty());
    }

    #[test]
    fn rename_merges_spellings_and_keeps_others() {
        let item =
            r#"{"Id":"a","Name":"x","Tags":["無碼\u200b","素人","無碼"],"People":[{"Name":"小美","Type":"Actor","Id":"p1"},{"Name":"導演","Type":"Director"},{"Name":"小美美","Type":"Actor"}]}"#;
        let t: Value = serde_json::from_str(&with_renamed(item, NameKind::Tag, "無碼\u{200b}", "無碼").unwrap()).unwrap();
        assert_eq!(t["Tags"], serde_json::json!(["無碼", "素人"]));
        assert_eq!(t["Name"], "x");
        let p: Value = serde_json::from_str(&with_renamed(item, NameKind::Person, " 小美", "小美美").unwrap()).unwrap();
        // 演員重新命名後和已經有的重複：只留一個；導演不動
        assert_eq!(p["People"], serde_json::json!([{"Name":"小美美","Type":"Actor"},{"Name":"導演","Type":"Director"}]));
        assert!(spelling_diff("無碼\u{200b}", "無碼").unwrap().contains("U+200B"));
        assert!(spelling_diff("無碼", "無碼\u{200b}").is_some());
        assert_eq!(spelling_diff("無碼", "無毛"), None);
        assert_eq!(spelling_diff("VR", "ｖｒ"), None);
    }

    #[test]
    fn rename_changes_only_videos_using_it() {
        let list = r#"{"TotalRecordCount":2,"Items":[{"Id":"a","Tags":["無碼 "]},{"Id":"b","Tags":["素人"]}]}"#.to_string();
        let item = r#"{"Id":"a","Name":"x","Tags":["無碼 "]}"#.to_string();
        let (url, h) = serve_seq(vec![("200 OK", list), ("200 OK", item), ("204 No Content", String::new())]);
        let jf = Jellyfin::new(&url, "k").unwrap();
        let seen = std::sync::Mutex::new(vec![]);
        let n = jf
            .rename_name("u", NameKind::Tag, "無碼", "無碼", &|d, t| {
                seen.lock().unwrap_or_else(|e| e.into_inner()).push((d, t));
                true
            })
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(*seen.lock().unwrap_or_else(|e| e.into_inner()), vec![(0, 1), (1, 1)]);
        let got = h.join().unwrap();
        assert!(got[1].starts_with("GET /Items/a "), "{got:?}");
        assert!(got[2].starts_with("POST /Items/a "), "{got:?}");
    }

    #[test]
    fn facet_query_or_within_and_across() {
        let q = facet_query(&[JfFacet::Person("p1".into(), "小美".into()), JfFacet::Year(2024), JfFacet::Person("p2".into(), "小明".into()), JfFacet::Tag("a,b".into()), JfFacet::Tag("旅行".into())]);
        assert_eq!(q, vec![("personIds", "p1,p2".to_string()), ("tags", "a,b|旅行".to_string()), ("years", "2024".to_string())]);
        assert!(facet_query(&[]).is_empty());
        assert_eq!(tally_years(vec![2021, 2024, 2021]), vec![(2024, 1), (2021, 2)]);
    }

    #[test]
    fn facets_drop_duplicate_tags() {
        let (tags, _) = facets_of(r#"{"Tags":["無碼","無碼 ","18+"," 旅行"],"Years":[2024]}"#);
        assert_eq!(tags, vec!["18+".to_string(), "旅行".to_string(), "無碼".to_string()]);
    }

    #[test]
    fn indexes_jellyfin_videos_by_file_name() {
        let libs = r#"[{"Name":"家庭影片","Locations":["/media/video"]},{"Name":"電影","Locations":["D:\\Movies"]}]"#;
        let items = r#"{"Items":[
            {"Id":"a","Path":"/media/video/family/2024/Clip.mp4","MediaSources":[{"Size":100}]},
            {"Id":"b","Path":"/media/video/top.mp4","MediaSources":[{"Size":5}]},
            {"Id":"c","Path":"D:\\Movies\\x\\Film.mkv","MediaSources":[{"Size":7}]},
            {"Id":"d","Path":"/other/place/odd.mp4","MediaSources":[{"Size":9}]},
            {"Id":"e","Path":"/media/video/nosize.mp4"}
        ]}"#;
        let idx = index_from_items(libs, items);
        assert_eq!(idx["clip.mp4"], vec![("家庭影片/family/2024".to_string(), 100)]);
        assert_eq!(idx["top.mp4"], vec![("家庭影片".to_string(), 5)]);
        assert_eq!(idx["film.mkv"], vec![("電影/x".to_string(), 7)]);
        assert_eq!(idx["odd.mp4"], vec![("place".to_string(), 9)]);
        assert!(!idx.contains_key("nosize.mp4"));
        // 從伺服器分批讀：2001 支要問兩次
        let page = |from: usize, n: usize| {
            let items: Vec<String> = (from..from + n).map(|i| format!(r#"{{"Id":"v{i}","Type":"Video","Path":"/media/video/v{i}.mp4","MediaSources":[{{"Size":{}}}]}}"#, i + 1)).collect();
            format!(r#"{{"TotalRecordCount":2001,"Items":[{}]}}"#, items.join(","))
        };
        let (url, h) = serve_seq(vec![("200 OK", libs.to_string()), ("200 OK", page(0, 2000)), ("200 OK", page(2000, 1))]);
        let idx = Jellyfin::new(&url, "k").unwrap().file_index().unwrap();
        assert_eq!(idx.len(), 2001);
        assert_eq!(idx["v2000.mp4"], vec![("家庭影片".to_string(), 2001)]);
        let req = h.join().unwrap();
        assert!(req[1].contains("startIndex=0&limit=2000") && req[2].contains("startIndex=2000&limit=2000"), "{req:?}");
    }

    #[test]
    fn finds_fields_jellyfin_did_not_keep() {
        let want = JfInfo { title: "生日".into(), overview: "蛋糕".into(), people: vec!["小明".into(), "阿嬤".into()], tags: vec!["家".into()], year: Some(2019) };
        // 順序、大小寫、前後空白不同不算
        let same = JfInfo { people: vec!["阿嬤 ".into(), "小明".into()], ..want.clone() };
        assert!(unsaved(&want, &same).is_empty());
        let lost = JfInfo { people: vec![], tags: vec![], year: Some(2026), ..want.clone() };
        assert_eq!(unsaved(&want, &lost), vec![tr!("年份", "year"), tr!("人物", "people"), tr!("標籤", "tags")]);
    }
}
