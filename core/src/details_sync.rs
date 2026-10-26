//! 本機錄影和 Jellyfin 上同一支影片（同檔名）的影片資訊雙向同步：標題、說明、年份、標籤、人物、只給大人看。
//! 作者、系列、類型、版權只存在本機的檔案裡，不同步。
//!
//! 哪一邊比較新：記下兩邊上次一致時的內容（details-sync.json），跟它比——
//! 只有一邊和上次不同，就是那一邊改過，以它為準；兩邊都改了同一個欄位而且不一樣，才算衝突（交給使用者選）。
//! 標籤、人物是清單：一邊加的、一邊刪的都算數。從來沒對過的（還沒有記錄）：一邊空的用另一邊的，清單合在一起。

use crate::jellyfin::{Jellyfin, JfInfo};
use crate::video_tags::{self, ADULT_KEY, ADULT_TAG, PEOPLE_KEY};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 兩邊共有的影片資訊
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Common {
    pub title: String,
    pub overview: String,
    /// 年份（四位數字；沒有時空的）
    pub year: String,
    pub tags: Vec<String>,
    pub people: Vec<String>,
    pub adult: bool,
}

/// 兩邊都改了、而且不一樣的欄位
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Title,
    Overview,
    Year,
}

impl Field {
    pub fn label(self) -> &'static str {
        match self {
            Field::Title => crate::tr!("標題", "Title"),
            Field::Overview => crate::tr!("說明", "Description"),
            Field::Year => crate::tr!("年份", "Year"),
        }
    }

    /// 本機影片資訊（video_tags）裡的欄位名稱
    pub fn local_key(self) -> &'static str {
        match self {
            Field::Title => "title",
            Field::Overview => "comment",
            Field::Year => "date",
        }
    }
}

impl Common {
    /// 本機影片資訊（video_tags::read 讀出的）
    pub fn from_local(m: &HashMap<String, String>) -> Common {
        let get = |k: &str| m.get(k).map(|v| v.trim().to_string()).unwrap_or_default();
        Common {
            title: get("title"),
            overview: get("comment"),
            year: year_of(&get("date")),
            tags: video_tags::split_genres(&get("keywords")),
            people: video_tags::split_genres(&get(PEOPLE_KEY)),
            adult: get(ADULT_KEY) == "1",
        }
    }

    /// Jellyfin 上的；標題和檔名一樣的是 Jellyfin 自己用檔名當的，當作沒有標題。標籤「18+」是只給大人看
    pub fn from_jf(info: &JfInfo, file_name: &str) -> Common {
        let stem = file_name.rsplit_once('.').map_or(file_name, |(s, _)| s);
        let title = info.title.trim();
        let title = if title.to_lowercase() == stem.trim().to_lowercase() { "" } else { title };
        let mut tags = vec![];
        let mut adult = false;
        for t in &info.tags {
            if t.trim() == ADULT_TAG {
                adult = true;
            } else if !t.trim().is_empty() {
                tags.push(t.trim().to_string());
            }
        }
        Common {
            title: title.to_string(),
            overview: info.overview.trim().to_string(),
            year: info.year.map(|y| y.to_string()).unwrap_or_default(),
            tags: dedup(tags),
            people: dedup(info.people.iter().map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()),
            adult,
        }
    }

    /// 內容一樣嗎（清單不管順序與大小寫）
    pub fn same(&self, o: &Common) -> bool {
        norm(&self.title) == norm(&o.title)
            && norm(&self.overview) == norm(&o.overview)
            && self.year == o.year
            && same_set(&self.tags, &o.tags)
            && same_set(&self.people, &o.people)
            && self.adult == o.adult
    }

    /// 換進本機影片資訊（其他欄位照舊）。年份：原本寫的是完整日期而年份一樣時不動
    pub fn to_local(&self, base: &HashMap<String, String>) -> HashMap<String, String> {
        let mut m = base.clone();
        m.insert("title".into(), self.title.clone());
        m.insert("comment".into(), self.overview.clone());
        if year_of(base.get("date").map(String::as_str).unwrap_or("")) != self.year {
            m.insert("date".into(), self.year.clone());
        }
        m.insert("keywords".into(), video_tags::join_genres(&self.tags));
        m.insert(PEOPLE_KEY.into(), video_tags::join_genres(&self.people));
        m.insert(ADULT_KEY.into(), if self.adult { "1".into() } else { String::new() });
        m
    }

    /// 換進 Jellyfin 的影片資訊。標題空的時用檔名（Jellyfin 一定要有名稱；沒有標題時它本來就用檔名）
    pub fn to_jf(&self, file_name: &str) -> JfInfo {
        let mut tags = self.tags.clone();
        if self.adult {
            tags.push(ADULT_TAG.into());
        }
        JfInfo {
            title: if self.title.is_empty() { file_name.rsplit_once('.').map_or(file_name, |(s, _)| s).to_string() } else { self.title.clone() },
            overview: self.overview.clone(),
            people: self.people.clone(),
            tags,
            year: self.year.parse().ok(),
        }
    }
}

/// 比對的結果
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// 合起來的結果（衝突的欄位先用本機的）
    pub merged: Common,
    /// 本機要改寫
    pub to_local: bool,
    /// Jellyfin 要更新
    pub to_jf: bool,
    /// 兩邊都改了、不一樣的欄位與 Jellyfin 上的內容
    pub conflicts: Vec<(Field, String)>,
}

/// 本機、Jellyfin 與上次一致時的內容（snap）比對，決定怎麼合
pub fn plan(local: &Common, jf: &Common, snap: Option<&Common>) -> Plan {
    let mut conflicts = vec![];
    let mut text = |f: Field, l: &str, j: &str, s: Option<&str>| -> String {
        if norm(l) == norm(j) {
            return l.to_string();
        }
        match s {
            Some(s) if norm(l) == norm(s) => j.to_string(),
            Some(s) if norm(j) == norm(s) => l.to_string(),
            None if l.trim().is_empty() => j.to_string(),
            None if j.trim().is_empty() => l.to_string(),
            _ => {
                conflicts.push((f, j.to_string()));
                l.to_string()
            }
        }
    };
    // Jellyfin 上沒有標題（用檔名當的）不是有人清掉：照本機的
    let jf_title = if jf.title.trim().is_empty() { &local.title } else { &jf.title };
    let title = text(Field::Title, &local.title, jf_title, snap.map(|s| s.title.as_str()));
    let overview = text(Field::Overview, &local.overview, &jf.overview, snap.map(|s| s.overview.as_str()));
    let year = text(Field::Year, &local.year, &jf.year, snap.map(|s| s.year.as_str()));
    let adult = match snap {
        _ if local.adult == jf.adult => local.adult,
        Some(s) if local.adult == s.adult => jf.adult,
        Some(_) => local.adult,
        None => true,
    };
    let merged = Common {
        title,
        overview,
        year,
        tags: merge_list(&local.tags, &jf.tags, snap.map(|s| s.tags.as_slice())),
        people: merge_list(&local.people, &jf.people, snap.map(|s| s.people.as_slice())),
        adult,
    };
    Plan { to_local: !merged.same(local), to_jf: !merged.same(jf), merged, conflicts }
}

/// 清單：兩邊都有的留著；上次沒有、現在有一邊有的是新加的，留著；上次有、有一邊拿掉的就拿掉
fn merge_list(l: &[String], j: &[String], s: Option<&[String]>) -> Vec<String> {
    let has = |list: &[String], x: &str| list.iter().any(|y| norm(y) == norm(x));
    let all: Vec<String> = l.iter().chain(j.iter()).cloned().collect();
    dedup(all.into_iter().filter(|x| (has(l, x) && has(j, x)) || !s.is_some_and(|s| has(s, x))).collect())
}

fn norm(s: &str) -> String {
    s.trim().replace("\r\n", "\n").to_lowercase()
}

fn same_set(a: &[String], b: &[String]) -> bool {
    let key = |l: &[String]| {
        let mut v: Vec<String> = l.iter().map(|x| norm(x)).filter(|x| !x.is_empty()).collect();
        v.sort();
        v.dedup();
        v
    };
    key(a) == key(b)
}

fn dedup(list: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for x in list {
        if !out.iter().any(|o| norm(o) == norm(&x)) {
            out.push(x);
        }
    }
    out
}

/// 「2026-05-01」「2026」→ 2026；看不懂的當作沒有
fn year_of(date: &str) -> String {
    let y: String = date.trim().chars().take(4).collect();
    if y.len() == 4 && y.chars().all(|c| c.is_ascii_digit()) {
        y
    } else {
        String::new()
    }
}

/// 上次一致時的內容：小寫檔名 → Common。存在設定資料夾的 details-sync.json；沒有 init 時（測試）只記在記憶體
static SNAPS: std::sync::Mutex<Option<(Option<PathBuf>, Snaps)>> = std::sync::Mutex::new(None);
/// 小寫檔名 → 上次一致時的內容
type Snaps = HashMap<String, Common>;

/// 最多記幾支（超過時隨便丟掉一些舊的；丟掉的下次當作沒對過，照樣合得起來）
const MAX_SNAPS: usize = 5000;

/// 讀出記下的內容（程式啟動時）
pub fn init(file: PathBuf) {
    let mut g = SNAPS.lock().unwrap_or_else(|e| e.into_inner());
    if g.as_ref().is_some_and(|(f, _)| f.is_some()) {
        return;
    }
    let mut map: HashMap<String, Common> = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    map.extend(g.take().map(|(_, m)| m).unwrap_or_default());
    *g = Some((Some(file), map));
}

/// 這支上次一致時的內容
pub fn snapshot(file_name: &str) -> Option<Common> {
    let g = SNAPS.lock().unwrap_or_else(|e| e.into_inner());
    g.as_ref().and_then(|(_, m)| m.get(&file_name.to_lowercase()).cloned())
}

/// 記下：兩邊現在都是這樣
pub fn remember(file_name: &str, c: &Common) {
    let mut g = SNAPS.lock().unwrap_or_else(|e| e.into_inner());
    let (file, map) = g.get_or_insert_with(|| (None, HashMap::new()));
    if map.get(&file_name.to_lowercase()) == Some(c) {
        return;
    }
    map.insert(file_name.to_lowercase(), c.clone());
    if map.len() > MAX_SNAPS {
        let extra: Vec<String> = map.keys().take(map.len() - MAX_SNAPS).cloned().collect();
        for k in extra {
            map.remove(&k);
        }
    }
    if let Some(file) = file.as_ref() {
        if let Err(e) = crate::paths::write_atomic(file, serde_json::to_string(map).unwrap_or_default().as_bytes()) {
            crate::warn!("[影片資訊同步] 記下內容失敗：{e}");
        }
    }
}

/// 改寫本機的影片資訊；大小變了，記下它還是 Jellyfin 上那一支（「已在 Jellyfin」照樣認得）
pub async fn write_local(ffmpeg: Option<&Path>, path: &str, values: &HashMap<String, String>) -> crate::Result<()> {
    let old = std::fs::metadata(path).map(|m| m.len()).ok();
    video_tags::write(ffmpeg, path, values).await?;
    if let (Some(old), Ok(m), Some(name)) = (old, std::fs::metadata(path), Path::new(path).file_name()) {
        crate::jellyfin::relink(&name.to_string_lossy(), old, m.len());
    }
    Ok(())
}

/// 本機一支和 Jellyfin 上同名那一支比對的結果
#[derive(Debug, Clone)]
pub struct Found {
    pub id: String,
    pub info: JfInfo,
    /// 本機的影片資訊（video_tags::read）
    pub local: HashMap<String, String>,
    pub plan: Plan,
}

fn file_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Jellyfin 上同名的那一支（沒有、或好幾支同名時 None）
pub async fn jf_twin(jf: &Jellyfin, user: &str, path: &str) -> Result<Option<(String, JfInfo)>, String> {
    let (jf, user, name) = (jf.clone(), user.to_string(), file_name(path));
    tokio::task::spawn_blocking(move || {
        let user = jf.browse_user(&user)?;
        jf.info_by_file(&user, &name)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 讀本機這一支與 Jellyfin 上同名的那一支，比對；Jellyfin 上沒有時 None
pub async fn check(jf: &Jellyfin, user: &str, ffmpeg: &Path, path: &str) -> Result<Option<Found>, String> {
    let local = video_tags::read(ffmpeg, path).await.map_err(|e| e.message().to_string())?;
    let Some((id, info)) = jf_twin(jf, user, path).await? else { return Ok(None) };
    Ok(Some(found(path, id, info, local)))
}

fn found(path: &str, id: String, info: JfInfo, local: HashMap<String, String>) -> Found {
    let name = file_name(path);
    let plan = plan(&Common::from_local(&local), &Common::from_jf(&info, &name), snapshot(&name).as_ref());
    Found { id, info, local, plan }
}

/// 兩邊照 merged 改（不一樣的那邊才改），都好了記下。回傳本機有沒有改寫
pub async fn apply(jf: &Jellyfin, user: &str, ffmpeg: &Path, path: &str, f: &Found, merged: &Common) -> Result<bool, String> {
    let name = file_name(path);
    let mut wrote = false;
    if !merged.same(&Common::from_local(&f.local)) {
        write_local(Some(ffmpeg), path, &merged.to_local(&f.local)).await.map_err(|e| e.message().to_string())?;
        wrote = true;
    }
    if !merged.same(&Common::from_jf(&f.info, &name)) {
        let (jf, user, id, info) = (jf.clone(), user.to_string(), f.id.clone(), merged.to_jf(&name));
        tokio::task::spawn_blocking(move || {
            let user = jf.browse_user(&user)?;
            jf.set_info(&user, &id, &info)
        })
        .await
        .map_err(|e| e.to_string())??;
    }
    remember(&name, merged);
    Ok(wrote)
}

/// 全部同步一次的結果
#[derive(Debug, Default, Clone)]
pub struct Report {
    /// 改寫了的本機檔案
    pub written: Vec<String>,
    /// 更新了幾支 Jellyfin 上的
    pub pushed: usize,
    /// 兩邊都改了、不知道用哪個的（打開本機的影片資訊時讓使用者選）
    pub conflicts: usize,
}

/// 確認過兩邊一致的本機檔案（路徑 → 大小、修改時間）：Jellyfin 上也沒變時不用再讀一次本機
static VERIFIED: std::sync::Mutex<Option<HashMap<String, Stamp>>> = std::sync::Mutex::new(None);
/// 檔案的大小、修改時間
type Stamp = (u64, Option<std::time::SystemTime>);

fn stamp(path: &str) -> Option<Stamp> {
    std::fs::metadata(path).ok().map(|m| (m.len(), m.modified().ok()))
}

/// 這些資料夾裡的 MP4 和 Jellyfin 上同名的影片全部對一次（兩邊都只有一支同名的才對）。
/// 只有一邊改過的照它改另一邊；兩邊都改了同一個欄位的跳過。一支一支來，本機要改寫整個檔案
pub async fn sync_all(jf: Jellyfin, user: String, ffmpeg: PathBuf, dirs: Vec<String>) -> Result<Report, String> {
    let jf2 = jf.clone();
    // 沒指定使用者時先換成管理員的編號，之後每支都用它（不用每支再問一次）
    let (user, remote) = tokio::task::spawn_blocking(move || Ok::<_, String>((jf2.browse_user(&user)?, jf2.all_infos()?))).await.map_err(|e| e.to_string())??;
    let mut by_name: HashMap<String, Vec<(String, JfInfo)>> = HashMap::new();
    for (id, name, info) in remote {
        by_name.entry(name.to_lowercase()).or_default().push((id, info));
    }
    let mut local: HashMap<String, Vec<String>> = HashMap::new();
    for d in &dirs {
        let Ok(rd) = std::fs::read_dir(d) else { continue };
        for e in rd.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_file())) {
            let path = e.path().display().to_string();
            if video_tags::can_tag(&path) {
                local.entry(e.file_name().to_string_lossy().to_lowercase()).or_default().push(path);
            }
        }
    }
    let mut report = Report::default();
    for (name, paths) in local {
        let ([path], Some([(id, info)])) = (paths.as_slice(), by_name.get(&name).map(Vec::as_slice)) else { continue };
        let snap = snapshot(&name);
        let now = stamp(path);
        let unchanged = snap.as_ref().is_some_and(|s| s.same(&Common::from_jf(info, &name)))
            && VERIFIED.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(path)).is_some_and(|v| Some(*v) == now);
        if unchanged {
            continue;
        }
        let values = match video_tags::read(&ffmpeg, path).await {
            Ok(v) => v,
            Err(e) => {
                crate::info!("[影片資訊同步] 讀不到 {path}：{}", e.message());
                continue;
            }
        };
        let f = found(path, id.clone(), info.clone(), values);
        if !f.plan.conflicts.is_empty() {
            report.conflicts += 1;
            continue;
        }
        let merged = f.plan.merged.clone();
        match apply(&jf, &user, &ffmpeg, path, &f, &merged).await {
            Ok(wrote) => {
                if wrote {
                    report.written.push(path.clone());
                }
                if f.plan.to_jf {
                    report.pushed += 1;
                }
                if let Some(st) = stamp(path) {
                    VERIFIED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(path.clone(), st);
                }
            }
            Err(e) => crate::warn!("[影片資訊同步] {path} 沒有同步好：{e}"),
        }
    }
    if !report.written.is_empty() || report.pushed > 0 || report.conflicts > 0 {
        crate::info!("[影片資訊同步] 本機改寫 {} 支、Jellyfin 更新 {} 支、兩邊都改過跳過 {} 支", report.written.len(), report.pushed, report.conflicts);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(title: &str, tags: &[&str]) -> Common {
        Common { title: title.into(), tags: tags.iter().map(|s| s.to_string()).collect(), ..Default::default() }
    }

    #[test]
    fn same_ignores_order_and_case() {
        assert!(c("A", &["x", "Y"]).same(&c("a ", &["y", "X"])));
        assert!(!c("A", &["x"]).same(&c("A", &["x", "z"])));
    }

    #[test]
    fn only_the_changed_side_wins() {
        let snap = c("舊", &["a"]);
        // Jellyfin 改了：本機跟著改
        let p = plan(&snap, &c("新", &["a", "b"]), Some(&snap));
        assert_eq!(p.merged, c("新", &["a", "b"]));
        assert!(p.to_local && !p.to_jf && p.conflicts.is_empty());
        // 本機改了：Jellyfin 跟著改
        let p = plan(&c("本機", &[]), &snap, Some(&snap));
        assert_eq!(p.merged, c("本機", &[]));
        assert!(!p.to_local && p.to_jf);
    }

    #[test]
    fn lists_merge_adds_and_removes_from_both_sides() {
        let snap = c("", &["a", "b"]);
        // 本機拿掉 a、加 c；Jellyfin 拿掉 b、加 d
        let p = plan(&c("", &["b", "c"]), &c("", &["a", "d"]), Some(&snap));
        assert!(same_set(&p.merged.tags, &["c".into(), "d".into()]));
        assert!(p.to_local && p.to_jf);
    }

    #[test]
    fn both_changed_title_is_a_conflict() {
        let snap = c("舊", &[]);
        let p = plan(&c("本機", &[]), &c("Jellyfin", &[]), Some(&snap));
        assert_eq!(p.conflicts, vec![(Field::Title, "Jellyfin".to_string())]);
        assert_eq!(p.merged.title, "本機");
    }

    #[test]
    fn never_synced_fills_empty_side_and_unions_lists() {
        let p = plan(&c("", &["a"]), &c("素人", &["b"]), None);
        assert_eq!(p.merged.title, "素人");
        assert!(same_set(&p.merged.tags, &["a".into(), "b".into()]));
        assert!(p.conflicts.is_empty());
        let p = plan(&c("情趣娃娃", &[]), &c("素人", &[]), None);
        assert_eq!(p.conflicts.len(), 1);
    }

    #[test]
    fn jellyfin_title_from_file_name_counts_as_empty() {
        let info = JfInfo { title: "Velfie_4_cut".into(), tags: vec!["18+".into(), "無碼".into()], year: Some(2025), ..Default::default() };
        let j = Common::from_jf(&info, "Velfie_4_cut.mp4");
        assert_eq!(j.title, "");
        assert!(j.adult);
        assert_eq!(j.tags, vec!["無碼".to_string()]);
        assert_eq!(j.year, "2025");
        // 再換回 Jellyfin：標題空的保留原本的，18+ 加回標籤
        let back = j.to_jf("Velfie_4_cut.mp4");
        assert_eq!(back.title, "Velfie_4_cut");
        assert!(back.tags.contains(&"18+".to_string()));
    }

    #[test]
    fn local_round_trip_keeps_other_fields_and_full_date() {
        let mut base = HashMap::new();
        base.insert("title".to_string(), "t".to_string());
        base.insert("album".to_string(), "人妻".to_string());
        base.insert("date".to_string(), "2025-05-01".to_string());
        let l = Common::from_local(&base);
        assert_eq!(l.year, "2025");
        let out = Common { title: "新".into(), people: vec!["素人".into()], ..l.clone() }.to_local(&base);
        assert_eq!(out["album"], "人妻");
        assert_eq!(out["date"], "2025-05-01");
        assert_eq!(out["title"], "新");
        assert_eq!(out[PEOPLE_KEY], "素人");
    }

    #[test]
    fn snapshots_are_remembered_by_file_name() {
        remember("Test_Snap.MP4", &c("x", &[]));
        assert_eq!(snapshot("test_snap.mp4"), Some(c("x", &[])));
    }
}
