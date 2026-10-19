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

    /// 把影片的標籤設成 tags（影片資訊的「標籤」；空的不動 Jellyfin 上的）：取回整筆資料、換掉 Tags 再存回去
    pub fn set_tags(&self, id: &str, tags: &[String]) -> Result<(), String> {
        if tags.is_empty() {
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
        let item = with_tags(&body, tags).ok_or_else(|| tr!("Jellyfin 的影片資料看不懂", "Couldn't understand Jellyfin's item data").to_string())?;
        self.call_body(agent().post(&format!("{}/Items/{id}", self.url)), &item).map(|_| ())
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

    /// 請 Jellyfin 產生章節縮圖（執行「擷取章節圖片」排程工作）。回傳媒體庫有沒有打開「擷取章節圖片」（沒打開時工作不會做）
    pub fn chapter_images(&self) -> Result<bool, String> {
        let tasks = self.call(agent().get(&format!("{}/ScheduledTasks?isHidden=false", self.url)))?.into_string().map_err(|e| e.to_string())?;
        let id = task_id(&tasks, "RefreshChapterImages").ok_or_else(|| tr!("這版 Jellyfin 沒有「擷取章節圖片」工作", "This Jellyfin has no chapter image task").to_string())?;
        self.call(agent().post(&format!("{}/ScheduledTasks/Running/{id}", self.url)))?;
        let libs = self.call(agent().get(&format!("{}/Library/VirtualFolders", self.url))).ok().and_then(|r| r.into_string().ok()).unwrap_or_default();
        Ok(chapter_images_enabled(&libs))
    }

    /// 傳好之後：等 Jellyfin 掃到這支（新檔案），設好標籤、請它做章節縮圖；回傳接在結果後面的說明
    pub fn after_upload(&self, file_name: &str, extras: &Extras, ids: Vec<String>, wait: Duration) -> String {
        let ids = if ids.is_empty() {
            match self.wait_for(file_name, wait) {
                Ok(Some(id)) => vec![id],
                Ok(None) => return tr!("（Jellyfin 還在掃描，稍後就會出現）", " (Jellyfin is still scanning; it'll show up shortly)").into(),
                Err(e) => return trf!("（查不到 Jellyfin 有沒有收到：{e}）", " (couldn't check whether Jellyfin got it: {e})"),
            }
        } else {
            ids
        };
        let mut notes = vec![tr!("✓ Jellyfin 已收到", "✓ Jellyfin has it").to_string()];
        if !extras.tags.is_empty() {
            let failed = ids.iter().filter_map(|id| self.set_tags(id, &extras.tags).err()).next();
            notes.push(match failed {
                None => trf!("標籤：{}", "tags: {}", extras.tags.join("、")),
                Some(e) => {
                    crate::warn!("[Jellyfin] 設定 {file_name} 的標籤失敗：{e}");
                    trf!("標籤沒有設好：{e}", "tags not set: {e}")
                }
            });
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
        self.refresh_items(file_name).map(|ids| ids.len())
    }

    /// 同 refresh_file，回傳重新整理的影片編號
    pub fn refresh_items(&self, file_name: &str) -> Result<Vec<String>, String> {
        let ids = self.find(file_name)?;
        if ids.is_empty() {
            self.refresh()?;
            return Ok(ids);
        }
        for id in &ids {
            let url = format!("{}/Items/{id}/Refresh?metadataRefreshMode=FullRefresh&imageRefreshMode=FullRefresh&replaceAllMetadata=true&replaceAllImages=true", self.url);
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
}

impl Extras {
    /// 從影片的 ffmetadata 讀出（keywords、[CHAPTER]）
    pub fn from_ffmetadata(text: &str) -> Extras {
        let tags = text.lines().find_map(|l| l.strip_prefix("keywords=").or_else(|| l.strip_prefix("KEYWORDS="))).map(|v| crate::video_tags::split_genres(&v.replace("\\;", ";"))).unwrap_or_default();
        Extras { tags, chapters: text.lines().any(|l| l.trim() == "[CHAPTER]") }
    }

    /// 讀影片（FFmpeg 讀不到時當作沒有）
    pub async fn read(ffmpeg: Option<&std::path::Path>, video: &std::path::Path) -> Extras {
        let Some(ff) = ffmpeg else { return Extras::default() };
        let v = video.display().to_string();
        let args = ["-hide_banner", "-loglevel", "error", "-i", v.as_str(), "-f", "ffmetadata", "-"];
        let r = crate::process::run(ff, &args, Duration::from_secs(30)).await;
        if r.code == 0 {
            Extras::from_ffmetadata(&r.stdout)
        } else {
            Extras::default()
        }
    }
}

/// 整筆影片資料換掉 Tags（其他欄位照原樣送回去，不然 Jellyfin 會清掉）
pub fn with_tags(item_json: &str, tags: &[String]) -> Option<String> {
    let mut v: Value = serde_json::from_str(item_json).ok()?;
    v.as_object_mut()?.insert("Tags".into(), Value::from(tags.to_vec()));
    serde_json::to_string(&v).ok()
}

/// 排程工作清單裡 Key 是 key 的工作編號
pub fn task_id(json: &str, key: &str) -> Option<String> {
    let v: Value = serde_json::from_str(json).ok()?;
    v.as_array()?.iter().find(|t| t.get("Key").and_then(Value::as_str) == Some(key)).and_then(|t| t.get("Id").and_then(Value::as_str)).map(String::from)
}

/// 有沒有任何一個媒體庫打開了「擷取章節圖片」
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
pub fn sync_edited(jf: Option<&Jellyfin>, copy_to: &str, video: &std::path::Path, extras: &Extras) -> Option<Result<String, String>> {
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
        if let Err(e) = crate::paths::copy_keep_time(video, &dest) {
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
    Some(match jf.refresh_items(&name) {
        Ok(ids) if ids.is_empty() => Ok(format!("{done}{}", tr!("Jellyfin 還沒有這支影片，已請它掃描媒體庫", "Jellyfin doesn't have this video yet, so it was asked to scan"))),
        Ok(ids) => {
            let more = jf.after_upload(&name, extras, ids, Duration::ZERO);
            Ok(format!("{done}{}{more}", tr!("已請 Jellyfin 重新讀取這支影片", "Jellyfin was asked to reload this video")))
        }
        Err(e) => {
            crate::warn!("[Jellyfin] 重新整理 {name} 失敗：{e}");
            Err(format!("{done}{}", trf!("但沒有通知到 Jellyfin：{e}", "but Jellyfin couldn't be notified: {e}")))
        }
    })
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
        let copied = std::fs::create_dir_all(lib).and_then(|_| {
            for f in &files {
                crate::paths::copy_keep_time(f, &lib.join(library_name(f)))?;
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
    Some(match jf.refresh_items(&name) {
        Ok(ids) => {
            let what = if ids.is_empty() { tr!("，已請 Jellyfin 掃描", ", Jellyfin asked to scan") } else { tr!("，已請 Jellyfin 重新讀取", ", Jellyfin asked to reload it") };
            let more = jf.after_upload(&name, extras, ids, Duration::from_secs(60));
            Ok(format!("{sent}{what}{more}"))
        }
        Err(e) => {
            crate::warn!("[Jellyfin] 通知 {name} 失敗：{e}");
            Err(format!("{sent}{}", trf!("，但沒有通知到 Jellyfin：{e}", ", but Jellyfin couldn't be notified: {e}")))
        }
    })
}

/// 檔案本來就放在「複製」的資料夾（媒體庫）裡：改名、刪除就是改到媒體庫那一份
pub fn in_library(copy_to: &str, file: &std::path::Path) -> bool {
    let copy_to = copy_to.trim();
    !copy_to.is_empty() && file.parent().is_some_and(|d| same_dir(d, std::path::Path::new(copy_to)))
}

/// 「複製」資料夾（媒體庫）裡屬於這個檔案的那一份：同名的檔案和它的字幕（同名.srt、同名.zh-TW.srt…）。
/// 檔案本來就在那個資料夾時是同一份，回傳空的；file 不用還在（改名、刪除之後也查得到）
pub fn library_copies(copy_to: &str, file: &std::path::Path) -> Vec<std::path::PathBuf> {
    let copy_to = copy_to.trim();
    let (Some(name), Some(stem)) = (file.file_name(), file.file_stem()) else { return vec![] };
    if copy_to.is_empty() || in_library(copy_to, file) {
        return vec![];
    }
    let (name, stem) = (name.to_string_lossy().to_lowercase(), stem.to_string_lossy().to_lowercase());
    let Ok(dir) = std::fs::read_dir(copy_to) else { return vec![] };
    let mut out: Vec<_> = dir
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| {
            let n = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            n == name || n.strip_prefix(&stem).is_some_and(|rest| rest.starts_with('.') && [".srt", ".vtt", ".ass"].iter().any(|x| rest.ends_with(x)))
        })
        .collect();
    out.sort();
    out
}

/// 改名後：媒體庫裡那一份（連同字幕）也換成新名稱；回傳改了幾個檔案
pub fn rename_copies(copy_to: &str, old: &std::path::Path, new: &std::path::Path) -> Result<usize, String> {
    let (Some(old_stem), Some(new_stem)) = (old.file_stem(), new.file_stem()) else { return Ok(0) };
    let skip = old_stem.to_string_lossy().chars().count();
    let new_stem = new_stem.to_string_lossy();
    let mut n = 0;
    for f in library_copies(copy_to, old) {
        let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let to = f.with_file_name(format!("{new_stem}{}", name.chars().skip(skip).collect::<String>()));
        if to.exists() {
            return Err(trf!("媒體庫裡已經有「{}」", "The library already has “{}”", to.file_name().unwrap_or_default().to_string_lossy()));
        }
        std::fs::rename(&f, &to).map_err(|e| e.to_string())?;
        crate::info!("[傳到資料夾] 跟著改名：{} → {}", f.display(), to.display());
        n += 1;
    }
    Ok(n)
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
        assert!(sync_edited(None, &copy_to, &video, &Extras::default()).is_none());
        assert!(sync_edited(None, "", &video, &Extras::default()).is_none());
        // 有同名的一份：換成改好的
        std::fs::write(lib.join("a.mp4"), "old").unwrap();
        assert!(sync_edited(None, &copy_to, &video, &Extras::default()).unwrap().is_ok());
        assert_eq!(std::fs::read_to_string(lib.join("a.mp4")).unwrap(), "new");
        // 直接改媒體庫裡的檔案（結尾有分隔符號、大小寫不同也算同一個資料夾）：不用複製，只通知
        let (url, h) = serve_seq(vec![("200 OK", r#"{"Items":[{"Id":"x","Path":"/m/a.mp4"}]}"#.to_string()), ("204 No Content", String::new())]);
        let jf = Jellyfin::new(&url, "k").unwrap();
        let msg = sync_edited(Some(&jf), &format!("{copy_to}/"), &lib.join("a.mp4"), &Extras::default()).unwrap().unwrap();
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
        assert_eq!(Extras::from_ffmetadata(meta), Extras { tags: vec!["報價".into(), "新人".into()], chapters: true });
        assert_eq!(Extras::from_ffmetadata(";FFMETADATA1\n"), Extras::default());
        let jf = Jellyfin::new("nas:8096", "k").unwrap();
        assert_eq!(jf.link("abc", Some("s1")), "http://nas:8096/web/#/details?id=abc&serverId=s1");
        assert_eq!(jf.link("abc", None), "http://nas:8096/web/#/details?id=abc");
        let item = with_tags(r#"{"Id":"abc","Name":"Rec","Genres":["教學"],"Tags":["old"]}"#, &["新".into()]).unwrap();
        let v: Value = serde_json::from_str(&item).unwrap();
        assert_eq!(v["Tags"], serde_json::json!(["新"]));
        assert_eq!(v["Genres"], serde_json::json!(["教學"]));
        assert_eq!(task_id(r#"[{"Key":"Other","Id":"1"},{"Key":"RefreshChapterImages","Id":"42"}]"#, "RefreshChapterImages").as_deref(), Some("42"));
        assert!(chapter_images_enabled(r#"[{"LibraryOptions":{"EnableChapterImageExtraction":false}},{"LibraryOptions":{"EnableChapterImageExtraction":true}}]"#));
        assert!(!chapter_images_enabled(r#"[{"LibraryOptions":{}}]"#));
        // 標籤：取回整筆、換掉 Tags 再送回去
        let (url, h) = serve_seq(vec![("200 OK", r#"{"Id":"abc","Name":"Rec","Tags":[]}"#.to_string()), ("204 No Content", String::new())]);
        Jellyfin::new(&url, "k").unwrap().set_tags("abc", &["新".into()]).unwrap();
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
        assert_eq!(names(library_copies(&copy_to, &rec.join("會議.mp4"))), ["會議.en.srt", "會議.mp4", "會議.zh-TW.srt"]);
        assert_eq!(names(library_copies(&copy_to, &rec.join("會議_2x.mp4"))), ["會議_2x.mp4"]);
        // 本來就在媒體庫裡：同一份，不算
        assert!(library_copies(&copy_to, &lib.join("會議.mp4")).is_empty());
        assert!(in_library(&format!("{copy_to}/"), &lib.join("會議.mp4")));
        assert!(library_copies("", &rec.join("會議.mp4")).is_empty());

        assert_eq!(rename_copies(&copy_to, &rec.join("會議.mp4"), &rec.join("週會.mp4")).unwrap(), 3);
        assert_eq!(names(library_copies(&copy_to, &rec.join("週會.mp4"))), ["週會.en.srt", "週會.mp4", "週會.zh-TW.srt"]);
        assert!(library_copies(&copy_to, &rec.join("會議.mp4")).is_empty());
        // 新名稱媒體庫裡已經有：不蓋掉
        assert!(rename_copies(&copy_to, &rec.join("週會.mp4"), &rec.join("其他.mp4")).is_err());
        assert_eq!(rename_copies(&copy_to, &rec.join("沒傳過.mp4"), &rec.join("新.mp4")).unwrap(), 0);
    }
}
