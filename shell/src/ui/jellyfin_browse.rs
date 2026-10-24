//! 檔案庫的「Jellyfin」分頁：列出伺服器上的媒體庫、資料夾、影片（家人用手機傳上去的也看得到），點了就在檢視器裡串流播放。
//! 不在家時只要連得到 Jellyfin 也能看。最上層先列「繼續觀看」「最近加入」；資料夾裡可以排序、只看沒看過的等。
//! 可以多選一次下載到這台電腦（下載回來就能剪輯）或刪除。見 core::jellyfin 的 views、items、stream_url

use super::theme::{self, Btn, Icon};
use super::{viewer, ToastAction, UiApp};
use eframe::egui::{self, pos2, vec2, Align, Align2, Layout, Rect, RichText, Sense, UiBuilder};
use screenrecorder_core::jellyfin::{self, Jellyfin, JfFacet, JfFilter, JfItem, JfKind, JfList, JfSort};
use screenrecorder_core::types::{Chapter, LibraryEntry, MediaInfo};
use screenrecorder_core::{tr, trf};
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

/// 一次讀幾個（捲到快底時再讀下一批）
const PAGE: usize = 60;
/// 每格最小的寬度（縮圖 16:9）：縮圖小、中、大
const CELL_W: [f32; 3] = [150.0, 196.0, 300.0];
/// 「繼續觀看」「最近加入」各讀幾個
const ROW_ITEMS: usize = 12;
/// 搜尋框停止打字多久之後才找（每打一個字就問伺服器太多次）
const SEARCH_WAIT: Duration = Duration::from_millis(400);
/// 最上層「合集」「播放清單」兩格的編號；點進一個合集、播放清單時位置的編號前面加 COL、PL
const COLLECTIONS: &str = "jf:collections";
const PLAYLISTS: &str = "jf:playlists";
const COL: &str = "jf:col:";
const PL: &str = "jf:pl:";

/// 位置的編號是一個合集或播放清單時：（哪一種, 它在 Jellyfin 上的編號）
fn list_at(id: &str) -> Option<(JfList, &str)> {
    id.strip_prefix(COL).map(|x| (JfList::Collection, x)).or_else(|| id.strip_prefix(PL).map(|x| (JfList::Playlist, x)))
}

/// 位置、縮圖用的編號在 Jellyfin 上真正的編號（去掉合集、播放清單的前綴）
fn real_id(id: &str) -> &str {
    list_at(id).map_or(id, |(_, x)| x)
}

/// 「依這些找」的範圍：目前所在的資料夾（含子資料夾）；最上層、「合集」「播放清單」那兩格裡是整個伺服器（None）
fn facet_scope(path: &[(String, String)]) -> Option<String> {
    path.last().map(|(id, _)| id.as_str()).filter(|id| *id != COLLECTIONS && *id != PLAYLISTS).map(|id| real_id(id).to_string())
}

/// 讀一層的一頁：最上層的「合集」「播放清單」列出全部合集、播放清單，播放清單裡照清單的順序，其他照一般的資料夾
#[allow(clippy::too_many_arguments)]
fn fetch(jf: &Jellyfin, user: &str, parent: Option<&str>, search: &str, start: usize, sort: JfSort, filter: JfFilter, facet: &[JfFacet]) -> Result<(Vec<JfItem>, usize), String> {
    let plain = search.trim().is_empty() && facet.is_empty();
    match parent {
        Some(p @ (COLLECTIONS | PLAYLISTS)) if plain => {
            let (kind, prefix) = if p == COLLECTIONS { (JfList::Collection, COL) } else { (JfList::Playlist, PL) };
            let all = jf.lists(user, kind)?;
            let n = all.len();
            Ok((all.into_iter().skip(start).take(PAGE).map(|i| JfItem { id: format!("{prefix}{}", i.id), ..i }).collect(), n))
        }
        Some(p) if plain && list_at(p).is_some_and(|(k, _)| k == JfList::Playlist) => jf.playlist_items(user, real_id(p), start, PAGE),
        // 「合集」「播放清單」那兩格裡依條件找：找整個伺服器
        Some(COLLECTIONS | PLAYLISTS) => jf.items(user, None, search, start, PAGE, sort, filter, facet),
        _ => jf.items(user, parent.map(real_id), search, start, PAGE, sort, filter, facet),
    }
}

/// 最上層「合集」「播放清單」那兩格
fn list_tiles(collections: usize, playlists: usize) -> [JfItem; 2] {
    [
        JfItem { id: COLLECTIONS.into(), name: tr!("合集", "Collections").into(), children: collections as u32, ..Default::default() },
        JfItem { id: PLAYLISTS.into(), name: tr!("播放清單", "Playlists").into(), children: playlists as u32, ..Default::default() },
    ]
}

enum Poster {
    Loading,
    Ready(egui::TextureHandle),
    None,
}

#[derive(Default)]
pub struct JfBrowse {
    /// 瀏覽用的使用者（讀到之前 None）
    user: Option<String>,
    /// 目前位置（編號, 名稱）；空的 = 最上層的媒體庫
    path: Vec<(String, String)>,
    /// 搜尋框的字、目前列出的是哪個搜尋
    search: String,
    searched: String,
    /// 上次打字的時間（停一下才找）
    search_at: Option<Instant>,
    items: Option<Vec<JfItem>>,
    /// 最上層的「繼續觀看」「最近加入」
    resume: Vec<JfItem>,
    latest: Vec<JfItem>,
    total: usize,
    /// 換了位置、搜尋、排序等時歸零（從第一批讀起）
    page: usize,
    /// 正在讀下一批（捲到快底時）
    loading_more: bool,
    /// 下一批讀不到（不再自動重試，換位置或重新整理時再讀）
    more_failed: bool,
    sort: JfSort,
    filter: JfFilter,
    /// 位置是從上次記住的還原的（那個資料夾讀不到時回到最上層）
    restored: bool,
    error: Option<String>,
    seq: u64,
    posters: HashMap<String, Poster>,
    /// 勾選的影片（編號 → 那支影片）
    selected: BTreeMap<String, JfItem>,
    /// 上次勾或取消勾的那一支（Shift＋按一下：從它選到點的那一支）
    anchor: Option<String>,
    /// 依人物、標籤、年份找（找目前資料夾底下、在最上層時整個伺服器的影片）：勾了的條件，同一類任一個符合、不同類都要符合；空的 = 沒有用
    facet: Vec<JfFacet>,
    /// 目前資料夾（含子資料夾；最上層是整個伺服器）的人物（編號, 名稱, 幾支影片）、標籤（名稱, 幾支）、年份（年, 幾支）
    /// （換資料夾時重讀，改了影片資訊後重讀）；在資料夾裡時沒用到的人物不列；
    /// 人物、標籤用得多的在前，年份新的在前；讀不到次數時是 0
    persons: Vec<(String, String, usize)>,
    tags: Vec<(String, usize)>,
    years: Vec<(u32, usize)>,
    /// 還沒設人物、還沒有標籤的影片幾支（「依這些找」最上面的「（沒有人物）」「（沒有標籤）」；讀不到時 0）
    no_people: usize,
    no_tags: usize,
    /// 伺服器上的合集、播放清單（「加進…」選單用；開啟分頁、加進去之後重讀）
    collections: Vec<JfItem>,
    playlists: Vec<JfItem>,
    /// 人物照片（人物編號 → 圖）：「依這些找」的人物清單用，看到才讀
    faces: HashMap<String, Poster>,
    /// 「人物」「標籤」「年份」選單裡的搜尋框
    facet_search: [String; 3],
    /// 位置那一列右邊（符合幾支、排序、縮圖大小）上次量到的寬度：左邊的條件只用剩下的地方
    crumb_right_w: f32,
    /// 上面那些是哪個範圍讀的（facet_scope；還沒讀過時 None）
    facets_for: Option<Option<String>>,
    /// 讀過的範圍：回到剛看過的資料夾不用再問伺服器（FACET_TTL 內；改了影片資訊時清掉）
    facet_cache: HashMap<Option<String>, (Instant, Facets)>,
    /// 讀不到標籤、年份時的原因（選單變灰時滑鼠移上去看得到）
    facets_error: Option<String>,
    /// 縮圖大小：0 小、1 中、2 大（記在設定裡）
    grid: u8,
    /// 用清單（表格）看，不是縮圖（記在設定裡）
    list_view: bool,
    /// 鍵盤移到的那一個（方向鍵移動、Enter 開啟、空白鍵勾選）
    cursor: Option<String>,
    /// 縮圖一排幾個（上次畫的；方向鍵上下用）
    cols: usize,
    /// 鍵盤移動了：捲到看得到那一個
    scroll_to_cursor: bool,
    /// 上次按 Ctrl 轉滾輪換縮圖大小的時間（轉一格只換一級）
    wheel_at: Option<Instant>,
}

impl JfBrowse {
    /// 最上層（媒體庫清單，沒有搜尋）
    fn at_top(&self) -> bool {
        self.path.is_empty() && self.searched.is_empty() && self.facet.is_empty()
    }

    /// 這支影片在畫面上的每一份（同一支可能同時在「繼續觀看」和資料夾裡）
    fn each_mut(&mut self, id: &str, mut f: impl FnMut(&mut JfItem)) {
        for it in self.items.iter_mut().flatten().chain(self.resume.iter_mut()).chain(self.latest.iter_mut()).filter(|i| i.id == id) {
            f(it);
        }
    }
}

/// 開始瀏覽（檔案庫切到「Jellyfin」分頁時；沒設定 Jellyfin 時提示到設定填）
pub fn open(app: &mut UiApp) {
    if app.jellyfin().is_none() {
        return app.toast(tr!("要先在「設定 → 存檔與分享」填 Jellyfin 的網址和 API 金鑰", "Fill in the Jellyfin address and API key in Settings → Saving & sharing first"), true);
    }
    let sort = JfSort::ALL.get(app.s.jf_sort as usize).copied().unwrap_or_default();
    let filter = JfFilter::ALL.get(app.s.jf_filter as usize).copied().unwrap_or_default();
    // 回到上次停的位置（媒體庫 › 資料夾…）
    let path = app.s.jf_path.clone();
    let restored = !path.is_empty();
    app.jf_browse = Some(JfBrowse { grid: app.s.jf_grid_size, list_view: app.s.jf_list_view, sort, filter, path, restored, cols: 1, ..Default::default() });
    load(app);
}

/// 讀到的一層：（使用者, 東西, 全部幾個, 繼續觀看, 最近加入, （合集, 播放清單）；不是最上層時 None）
type Loaded = (String, Vec<JfItem>, usize, Vec<JfItem>, Vec<JfItem>, Option<(Vec<JfItem>, Vec<JfItem>)>);

/// 讀目前這一層（或搜尋結果）的這一頁；最上層連「繼續觀看」「最近加入」一起讀
fn load(app: &mut UiApp) {
    let Some(jf) = app.jellyfin() else { return };
    let preferred = app.s.jellyfin_user.clone();
    let Some(d) = app.jf_browse.as_mut() else { return };
    d.seq += 1;
    d.items = None;
    d.error = None;
    d.loading_more = false;
    d.more_failed = false;
    d.searched = d.search.trim().to_string();
    let (seq, user, parent, search, start, sort, filter, facet) =
        (d.seq, d.user.clone(), d.path.last().map(|(id, _)| id.clone()), d.searched.clone(), d.page * PAGE, d.sort, d.filter, d.facet.clone());
    let want_facets = d.facets_for.as_ref() != Some(&facet_scope(&d.path));
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || -> Result<Loaded, String> {
                let user = match user {
                    Some(u) => u,
                    None => jf.browse_user(&preferred)?,
                };
                if parent.is_none() && search.is_empty() && facet.is_empty() {
                    let mut views = jf.views(&user)?;
                    // 舊版 Jellyfin 沒有這些也沒關係：只是不列
                    let resume = jf.resume_items(&user, ROW_ITEMS).unwrap_or_default();
                    let latest = jf.latest_items(&user, ROW_ITEMS).unwrap_or_default();
                    let cols = jf.lists(&user, JfList::Collection).unwrap_or_default();
                    let pls = jf.lists(&user, JfList::Playlist).unwrap_or_default();
                    views.extend(list_tiles(cols.len(), pls.len()));
                    let n = views.len();
                    return Ok((user, views, n, resume, latest, Some((cols, pls))));
                }
                let (items, total) = fetch(&jf, &user, parent.as_deref(), &search, start, sort, filter, &facet)?;
                Ok((user, items, total, vec![], vec![], None))
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        },
        move |app, r| {
            let Some(d) = app.jf_browse.as_mut().filter(|d| d.seq == seq) else { return };
            // 上次記住的資料夾讀不到或空了（被刪掉、搬走、改了權限）：回到最上層
            let gone = match &r {
                Err(_) => true,
                Ok((_, items, ..)) => items.is_empty(),
            };
            if gone && std::mem::take(&mut d.restored) && !d.path.is_empty() {
                d.path.clear();
                d.page = 0;
                return load(app);
            }
            d.restored = false;
            match r {
                Ok((user, items, total, resume, latest, lists)) => {
                    d.user = Some(user);
                    d.items = Some(items);
                    d.total = total;
                    d.resume = resume;
                    d.latest = latest;
                    if let Some((cols, pls)) = lists {
                        d.collections = cols;
                        d.playlists = pls;
                    }
                }
                Err(e) => d.error = Some(e),
            }
        },
    );
    if want_facets && !facets_from_cache(app) {
        load_facets(app);
    }
}

/// 「依這些找」的人物、標籤、年份在多久內不重讀（改了影片資訊會馬上重讀）
const FACET_TTL: Duration = Duration::from_secs(300);

/// 這個資料夾剛讀過：直接用，不問伺服器（有勾人物時照樣重讀：勾著的要留在清單上）
fn facets_from_cache(app: &mut UiApp) -> bool {
    let Some(d) = app.jf_browse.as_mut() else { return false };
    if d.facet.iter().any(|f| matches!(f, JfFacet::Person(..))) {
        return false;
    }
    let scope = facet_scope(&d.path);
    let Some((_, (persons, facets))) = d.facet_cache.get(&scope).filter(|(at, _)| at.elapsed() < FACET_TTL).cloned() else { return false };
    d.facets_for = Some(scope);
    apply_facets(d, persons, facets);
    true
}

fn apply_facets(d: &mut JfBrowse, persons: Vec<(String, String, usize)>, facets: FacetCounts) {
    d.persons = persons;
    match facets {
        Ok((tags, years, no_people, no_tags)) => {
            d.tags = tags;
            d.years = years;
            d.no_people = no_people;
            d.no_tags = no_tags;
            d.facets_error = None;
        }
        Err(e) => d.facets_error = Some(e),
    }
}

/// 改了影片資訊：每個資料夾的人物、標籤、年份都可能變了，全部重讀
fn refresh_facets(app: &mut UiApp) {
    jellyfin::forget_lacking();
    if let Some(d) = app.jf_browse.as_mut() {
        d.facet_cache.clear();
    }
    load_facets(app);
}

/// 讀到的人物（編號, 名稱, 幾支）與（標籤、年份）或讀不到的原因
type Facets = (Vec<(String, String, usize)>, FacetCounts);
/// 標籤（名稱, 幾支）、年份（年, 幾支）、沒有人物的幾支、沒有標籤的幾支；或讀不到的原因
type FacetCounts = Result<(Vec<(String, usize)>, Vec<(u32, usize)>, usize, usize), String>;

/// 讀伺服器上的人物、標籤、年份和各用在幾支影片（「依這些找」的選單）；舊版 Jellyfin 讀不到就不列。
/// 次數讀不到時改問只有名稱的清單
fn load_facets(app: &mut UiApp) {
    let Some(jf) = app.jellyfin() else { return };
    let preferred = app.s.jellyfin_user.clone();
    let Some(d) = app.jf_browse.as_mut() else { return };
    let user = d.user.clone();
    let scope = facet_scope(&d.path);
    d.facets_for = Some(scope.clone());
    // 勾著的人物在這個資料夾裡沒有也留著（選單裡才拿得掉）
    let chosen: Vec<String> = d.facet.iter().filter_map(|f| if let JfFacet::Person(id, _) = f { Some(id.clone()) } else { None }).collect();
    let for_scope = scope.clone();
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || {
                let user = match user {
                    Some(u) => u,
                    None => jf.browse_user(&preferred).unwrap_or_default(),
                };
                let persons = jf.persons(&user).unwrap_or_default();
                let (persons, rest) = match jf.usage_in(&user, scope.as_deref()) {
                    Ok(u) => {
                        let count: HashMap<String, usize> = u.people.iter().map(|(n, c)| (jellyfin::name_key(n), *c)).collect();
                        let mut persons: Vec<_> = persons.into_iter().map(|(id, n)| (id, count.get(&jellyfin::name_key(&n)).copied().unwrap_or(0), n)).collect();
                        // 在資料夾裡：只列這個資料夾用到的人物
                        if scope.is_some() {
                            persons.retain(|(id, c, _)| *c > 0 || chosen.contains(id));
                        }
                        // 用得多的在前（persons 已經依名稱排，sort_by 是穩定的）
                        persons.sort_by_key(|a| std::cmp::Reverse(a.1));
                        (persons.into_iter().map(|(id, c, n)| (id, n, c)).collect(), Ok((u.tags, u.years, u.no_people, u.no_tags)))
                    }
                    Err(e) => {
                        screenrecorder_core::info!("[Jellyfin] 讀不到人物、標籤、年份的使用次數：{e}");
                        let rest = jf.facets(&user).map(|(t, y)| (t.into_iter().map(|t| (t, 0)).collect(), y.into_iter().map(|y| (y, 0)).collect(), 0, 0));
                        (persons.into_iter().map(|(id, n)| (id, n, 0)).collect(), rest)
                    }
                };
                (persons, rest)
            })
            .await
            .unwrap_or_else(|e| (vec![], Err(e.to_string())))
        },
        move |app, (persons, facets): Facets| {
            // 讀好之前又換了資料夾：丟掉（換的時候已經重讀）
            if let Some(d) = app.jf_browse.as_mut().filter(|d| d.facets_for.as_ref() == Some(&for_scope)) {
                if facets.is_ok() {
                    d.facet_cache.insert(for_scope, (Instant::now(), (persons.clone(), facets.clone())));
                }
                apply_facets(d, persons, facets);
            }
        },
    );
}

/// 人物、標籤重新命名之後重讀「依這些找」的清單（Jellyfin 分頁沒開時不用）
pub fn reload_facets(app: &mut UiApp) {
    if app.jf_browse.is_some() {
        refresh_facets(app);
    }
}

/// 在「影片資訊」改好了：名稱跟著換、縮圖重讀（可能換了封面），人物、標籤重讀；正在播這支的話標題也換
pub fn edited(app: &mut UiApp, id: &str, title: &str) {
    super::jellyfin_face::info_saved(app, id);
    if let Some(v) = app.viewer.as_mut().filter(|_| !title.trim().is_empty()) {
        for e in v.list.iter_mut().filter(|e| Jellyfin::item_of_stream(&e.media.path).as_deref() == Some(id)) {
            let ext = e.media.name.rsplit_once('.').map(|(_, x)| x.to_string()).unwrap_or_else(|| "mp4".into());
            e.media.name = format!("{}.{ext}", title.trim());
        }
    }
    let Some(d) = app.jf_browse.as_mut() else { return };
    if !title.trim().is_empty() {
        d.each_mut(id, |it| it.name = title.trim().to_string());
    }
    d.posters.remove(id);
    refresh_facets(app);
}

/// 「影片資訊」存好了：清單上那支的人物、標籤跟著換；正在看「沒有人物」「沒有標籤」而這支現在有了，就從清單移除
/// （「符合 N 支」減 1，鍵盤游標移到下一支）
pub fn info_saved(app: &mut UiApp, id: &str, people: &[String], tags: &[String]) {
    let Some(d) = app.jf_browse.as_mut() else { return };
    d.each_mut(id, |it| {
        it.people = people.to_vec();
        it.tags = tags.to_vec();
    });
    let gone = (d.facet.contains(&JfFacet::NoPeople) && !people.is_empty()) || (d.facet.contains(&JfFacet::NoTags) && !tags.is_empty());
    let Some(items) = d.items.as_mut().filter(|_| gone) else { return };
    let Some(i) = items.iter().position(|it| it.id == id) else { return };
    items.remove(i);
    d.total = d.total.saturating_sub(1);
    d.selected.remove(id);
    if d.cursor.as_deref() == Some(id) {
        d.cursor = items.get(i).or_else(|| i.checked_sub(1).and_then(|j| items.get(j))).map(|it| it.id.clone());
    }
}

/// 好幾支一起整理完了：清單上（和勾選的）那些影片的人物、標籤、年份跟著換，「依這些找」重讀
pub fn batch_done(app: &mut UiApp, ids: &[String], edit: &jellyfin::BatchEdit) {
    let Some(d) = app.jf_browse.as_mut() else { return };
    let apply = |it: &mut JfItem| {
        let key = jellyfin::name_key;
        let change = |list: &mut Vec<String>, add: &[String], remove: &[String]| {
            list.retain(|n| !remove.iter().any(|r| key(r) == key(n)));
            for a in add {
                if !list.iter().any(|n| key(n) == key(a)) {
                    list.push(a.trim().to_string());
                }
            }
        };
        change(&mut it.people, &edit.add_people, &edit.remove_people);
        change(&mut it.tags, &edit.add_tags, &edit.remove_tags);
        if let Some(y) = edit.year {
            it.year = y;
        }
    };
    for id in ids {
        d.each_mut(id, apply);
        if let Some(it) = d.selected.get_mut(id) {
            apply(it);
        }
    }
    // 正在看「沒有人物」「沒有標籤」：現在有了的從清單移除（和單支存好時一樣）
    let (no_people, no_tags) = (d.facet.contains(&JfFacet::NoPeople), d.facet.contains(&JfFacet::NoTags));
    if let Some(items) = d.items.as_mut().filter(|_| no_people || no_tags) {
        let before = items.len();
        items.retain(|it| !(ids.contains(&it.id) && ((no_people && !it.people.is_empty()) || (no_tags && !it.tags.is_empty()))));
        d.total = d.total.saturating_sub(before - items.len());
        let left: std::collections::HashSet<&String> = items.iter().map(|it| &it.id).collect();
        if d.cursor.as_ref().is_some_and(|c| !left.contains(c)) {
            d.cursor = None;
        }
    }
    refresh_facets(app);
}

/// 這支影片的人物與它們在伺服器上的編號（人物照片用）
pub fn people_of(app: &UiApp, id: &str) -> Vec<(Option<String>, String)> {
    let Some(d) = app.jf_browse.as_ref() else { return vec![] };
    let names = d.items.iter().flatten().chain(d.resume.iter()).chain(d.latest.iter()).find(|i| i.id == id).map(|i| i.people.clone()).unwrap_or_default();
    super::jellyfin_face::people_with_ids(&names, &d.persons)
}

/// 換了人物照片：清單上的那張重讀
pub fn face_changed(app: &mut UiApp, person: &str) {
    if let Some(d) = app.jf_browse.as_mut() {
        d.faces.remove(person);
    }
}

/// 讀一個人物的照片（「依這些找」的人物清單；沒有照片時記成沒有，不再問）
fn load_face(app: &mut UiApp, id: &str) {
    let Some(jf) = app.jellyfin() else { return };
    let Some(d) = app.jf_browse.as_mut() else { return };
    d.faces.insert(id.to_string(), Poster::Loading);
    let (id, ctx) = (id.to_string(), app.ctx.clone());
    app.spawn(
        async move {
            let key = id.clone();
            let img = tokio::task::spawn_blocking(move || {
                let pm = screenrecorder_core::picture::decode_pixmap(&jf.poster(&key)?)?;
                Some(egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data()))
            })
            .await
            .ok()
            .flatten();
            (id.clone(), img.map(|i| ctx.load_texture(format!("jf-face-{id}"), i, egui::TextureOptions::LINEAR)))
        },
        |app, (id, tex): (String, Option<egui::TextureHandle>)| {
            if let Some(d) = app.jf_browse.as_mut() {
                d.faces.insert(id, tex.map_or(Poster::None, Poster::Ready));
                evict(&mut d.faces, &FACE_SEEN);
            }
        },
    );
}

/// 伺服器上已經有的人物、標籤（影片資訊對話框按一下加入）；Jellyfin 分頁沒開時空的
pub fn known_people_tags(app: &UiApp) -> (Vec<String>, Vec<String>) {
    app.jf_browse.as_ref().map(|d| (d.persons.iter().map(|(_, n, _)| n.clone()).collect(), d.tags.iter().map(|(t, _)| t.clone()).collect())).unwrap_or_default()
}

/// 在播放畫面按了星號：清單上的那一支也跟著改
pub fn set_favorite(app: &mut UiApp, id: &str, on: bool) {
    if let Some(d) = app.jf_browse.as_mut() {
        d.each_mut(id, |it| it.favorite = on);
    }
}

/// 讀這一格的縮圖（背景下載、轉成材質）
fn load_poster(app: &mut UiApp, id: &str) {
    let Some(jf) = app.jellyfin() else { return };
    let Some(d) = app.jf_browse.as_mut() else { return };
    d.posters.insert(id.to_string(), Poster::Loading);
    let id = id.to_string();
    let ctx = app.ctx.clone();
    app.spawn(
        async move {
            let key = id.clone();
            let img = tokio::task::spawn_blocking(move || {
                let bytes = jf.poster(real_id(&key))?;
                let pm = screenrecorder_core::picture::decode_pixmap(&bytes)?;
                Some(egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data()))
            })
            .await
            .ok()
            .flatten();
            (id, img.map(|i| ctx.load_texture("jf-poster", i, egui::TextureOptions::LINEAR)))
        },
        |app, (id, tex)| {
            if let Some(d) = app.jf_browse.as_mut() {
                d.posters.insert(id, tex.map_or(Poster::None, Poster::Ready));
                // 每張都是一個材質（一張約 0.4～1 MB）：超過 300 張只留最近畫過的 200 張，捲回去時再讀
                evict(&mut d.posters, &POSTER_SEEN);
            }
        },
    );
}

thread_local! {
    /// 每張封面、頭像最後一次畫出來是第幾次（清掉太久沒看的用）
    static POSTER_SEEN: std::cell::RefCell<(u64, HashMap<String, u64>)> = std::cell::RefCell::new((0, HashMap::new()));
    static FACE_SEEN: std::cell::RefCell<(u64, HashMap<String, u64>)> = std::cell::RefCell::new((0, HashMap::new()));
}

type Seen = std::thread::LocalKey<std::cell::RefCell<(u64, HashMap<String, u64>)>>;

/// 記下這張剛畫過
fn seen(which: &'static Seen, id: &str) {
    which.with_borrow_mut(|(tick, m)| {
        *tick += 1;
        match m.get_mut(id) {
            Some(t) => *t = *tick,
            None => drop(m.insert(id.to_string(), *tick)),
        }
    });
}

/// 超過 300 張時只留最近畫過的 200 張（還在讀的不動）
fn evict(map: &mut HashMap<String, Poster>, which: &'static Seen) {
    if map.len() <= 300 {
        return;
    }
    which.with_borrow_mut(|(_, m)| {
        let mut ids: Vec<(u64, String)> = map.iter().filter(|(_, p)| !matches!(p, Poster::Loading)).map(|(k, _)| (m.get(k).copied().unwrap_or(0), k.clone())).collect();
        ids.sort_unstable_by_key(|x| std::cmp::Reverse(x.0));
        for (_, k) in ids.into_iter().skip(200) {
            map.remove(&k);
            m.remove(&k);
        }
    });
}

/// 從 Jellyfin 刪掉這支影片（先確認）：伺服器上的檔案一起刪，不能復原；這台電腦上的檔案不動
pub fn confirm_delete(app: &mut UiApp, id: String, name: String) {
    confirm_delete_many(app, vec![(id, name)]);
}

/// 一次刪掉好幾支（找重複的視窗用）
pub fn confirm_delete_list(app: &mut UiApp, list: Vec<(String, String)>) {
    confirm_delete_many(app, list);
}

/// 一次刪掉好幾支（先確認一次）。刪好之後瀏覽清單移除它們，檢視器正在播其中一支就關掉
fn confirm_delete_many(app: &mut UiApp, list: Vec<(String, String)>) {
    let msg = tr!(
        "會從 Jellyfin（連同伺服器上的影片檔）刪掉，不會進資源回收筒，刪了不能復原。這台電腦上的檔案不會動。",
        "This deletes them from Jellyfin (including the video files on the server). They don't go to a Recycle Bin and can't be undone. Files on this computer aren't touched."
    );
    let title = if list.len() == 1 {
        tr!("從 Jellyfin 刪除這支影片？", "Delete this video from Jellyfin?").to_string()
    } else {
        trf!("從 Jellyfin 刪除這 {} 支影片？", "Delete these {} videos from Jellyfin?", list.len())
    };
    let names: Vec<String> = list.iter().map(|(_, n)| n.clone()).collect();
    let mut ask = super::dialogs::Ask::confirm(title, msg, tr!("刪除", "Delete"), move |app, _| {
        let Some(jf) = app.jellyfin() else { return };
        let todo = list.clone();
        // 一支一支刪，好幾支時要一點時間：先說正在刪，刪好再說結果
        app.toast(
            if list.len() == 1 {
                trf!("正在從 Jellyfin 刪除：{}", "Deleting from Jellyfin: {}", list[0].1)
            } else {
                trf!("正在從 Jellyfin 刪除 {} 支影片…", "Deleting {} videos from Jellyfin…", list.len())
            },
            false,
        );
        app.spawn(
            async move { tokio::task::spawn_blocking(move || todo.into_iter().map(|(id, name)| (jf.delete_item(&id).map(|_| id), name)).collect::<Vec<_>>()).await.unwrap_or_default() },
            move |app, done| {
                let ok: Vec<String> = done.iter().filter_map(|(r, _)| r.as_ref().ok().cloned()).collect();
                super::jellyfin_dupes::removed(app, &ok);
                if let Some(d) = app.jf_browse.as_mut() {
                    for list in [d.items.as_mut(), Some(&mut d.resume), Some(&mut d.latest)].into_iter().flatten() {
                        list.retain(|i| !ok.contains(&i.id));
                    }
                    d.total = d.total.saturating_sub(ok.len());
                    d.selected.retain(|id, _| !ok.contains(id));
                }
                if app.viewer.as_ref().is_some_and(|v| Jellyfin::item_of_stream(&v.cur().media.path).is_some_and(|id| ok.contains(&id))) {
                    app.viewer = None;
                }
                // NAS 上的檔案少了：「Jellyfin」標籤重讀
                app.lib_index = None;
                app.jf_watch = None;
                match done.iter().find_map(|(r, name)| r.as_ref().err().map(|e| (e, name))) {
                    Some((e, name)) => app.toast(trf!("「{name}」沒有刪掉：{e}", "Couldn't delete “{name}”: {e}"), true),
                    None if ok.len() == 1 => app.toast(trf!("已從 Jellyfin 刪除：{}", "Deleted from Jellyfin: {}", done[0].1), false),
                    None => app.toast(trf!("已從 Jellyfin 刪除 {} 支影片", "Deleted {} videos from Jellyfin", ok.len()), false),
                }
            },
        );
    });
    ask.list = names;
    ask.danger = true;
    app.ask = Some(ask);
}

/// 下載到這台電腦的儲存位置（下載回來就能剪輯、做加速版）：先寫暫存檔，右下角顯示進度、可以取消。
/// 同名同大小的已經下載過就不再下載；同名但不同的另取「名稱 (2)」
pub fn download(app: &mut UiApp, list: Vec<(String, String)>) {
    let Some(jf) = app.jellyfin() else { return };
    if list.is_empty() {
        return;
    }
    let dir = std::path::PathBuf::from(app.s.out_dir(&app.env));
    app.toast(if list.len() == 1 { trf!("正在下載：{}", "Downloading: {}", list[0].1) } else { trf!("正在下載 {} 支影片", "Downloading {} videos", list.len()) }, false);
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || {
                let mut out = vec![];
                for (id, file) in list {
                    let r = match free_name(&dir, &file) {
                        Some(to) => jf.download(&id, &to).map(|_| Some(to)),
                        None => Ok(None),
                    };
                    let cancelled = matches!(&r, Err(e) if screenrecorder_core::transfer::is_canceled(e));
                    out.push((file, r));
                    if cancelled {
                        break;
                    }
                }
                out
            })
            .await
            .unwrap_or_default()
        },
        |app, done| {
            let got: Vec<std::path::PathBuf> = done.iter().filter_map(|(_, r)| r.as_ref().ok().cloned().flatten()).collect();
            if let Some(d) = app.library.as_mut() {
                d.dirty = true;
            }
            app.load_recent();
            if let Some((file, Err(e))) = done.iter().find(|(_, r)| r.is_err()) {
                // 自己按了取消：不算錯誤，說一聲下載好幾支就好
                if screenrecorder_core::transfer::is_canceled(e) {
                    return app.toast(
                        if got.is_empty() {
                            tr!("已取消下載", "Download canceled").to_string()
                        } else {
                            trf!("已取消下載（已下載好 {} 支）", "Download canceled ({} already downloaded)", got.len())
                        },
                        false,
                    );
                }
                return app.toast(trf!("「{file}」沒有下載完：{e}", "“{file}” wasn't downloaded: {e}"), true);
            }
            let text = match got.len() {
                0 => tr!("已經下載過了，在儲存位置裡", "Already downloaded; it's in your save folder").to_string(),
                1 => trf!("已下載到這台電腦：{}", "Downloaded to this computer: {}", got[0].file_name().unwrap_or_default().to_string_lossy()),
                n => trf!("已下載 {n} 支影片到這台電腦", "Downloaded {n} videos to this computer"),
            };
            let first = got.first().map(|p| p.display().to_string());
            let actions = first
                .map(|p| {
                    vec![ToastAction::new(tr!("在資料夾中顯示", "Show in folder"), move |app| {
                        app.act(super::EntryAction::Reveal, LibraryEntry { media: MediaInfo { path: p.clone(), ..Default::default() }, exports: vec![], hits: vec![] })
                    })]
                })
                .unwrap_or_default();
            app.toast_with(text, false, actions);
        },
    );
}

/// 先下載到這台電腦，下載好接著剪輯、製作（已經下載過的直接用那一份）
pub fn download_then(app: &mut UiApp, item: (String, String), then: super::EntryAction) {
    let Some(jf) = app.jellyfin() else { return };
    if let Some(why) = app.blocked(then) {
        return app.toast(why, true);
    }
    let dir = std::path::PathBuf::from(app.s.out_dir(&app.env));
    let (id, file) = item;
    app.toast(trf!("正在下載「{file}」，下載好就開啟", "Downloading “{file}”; it opens when done"), false);
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || -> Result<std::path::PathBuf, String> {
                match free_name(&dir, &file) {
                    Some(to) => jf.download(&id, &to).map(|_| to),
                    None => Ok(dir.join(safe_file_name(&file))),
                }
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        },
        move |app, r| match r {
            Ok(path) => {
                if let Some(d) = app.library.as_mut() {
                    d.reload();
                }
                app.load_recent();
                // 剪輯、製作視窗關掉後回到原本的播放畫面
                app.leave_for_edit();
                app.act_path(then, path.display().to_string());
            }
            Err(e) if screenrecorder_core::transfer::is_canceled(&e) => app.toast(tr!("已取消下載", "Download canceled"), false),
            Err(e) => app.toast(trf!("沒有下載完：{e}", "The download didn't finish: {e}"), true),
        },
    );
}

/// 下載的檔名：還沒有就用原本的；同名同大小（已經下載過）None；同名不同的加上 (2)、(3)…
fn free_name(dir: &std::path::Path, file: &str) -> Option<std::path::PathBuf> {
    let file = safe_file_name(file);
    let file = file.as_str();
    let first = dir.join(file);
    if !first.exists() {
        return Some(first);
    }
    let (stem, ext) = file.rsplit_once('.').map_or((file, String::new()), |(s, e)| (s, format!(".{e}")));
    (2..1000).map(|i| dir.join(format!("{stem} ({i}){ext}"))).find(|p| !p.exists())
}

/// 伺服器給的檔名只當作名字用：移除資料夾（/、\\、..）與 Windows 不能用的字元；
/// 副檔名不是影片的（例如 .exe、.lnk）加上 .mp4，下載的東西不會變成可以執行的檔案
fn safe_file_name(file: &str) -> String {
    const VIDEO: [&str; 22] = ["mp4", "m4v", "mkv", "mov", "avi", "wmv", "webm", "ts", "mts", "m2ts", "flv", "f4v", "mpg", "mpeg", "3gp", "ogv", "vob", "rm", "rmvb", "asf", "divx", "mxf"];
    let last = file.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = last.chars().map(|c| if c.is_control() || "<>:\"|?*".contains(c) { '_' } else { c }).collect();
    let clean = clean.trim().trim_start_matches('.').trim_end_matches(['.', ' ']).to_string();
    if clean.is_empty() {
        return "jellyfin.mp4".into();
    }
    match clean.rsplit_once('.') {
        Some((_, ext)) if VIDEO.contains(&ext.to_ascii_lowercase().as_str()) => clean,
        _ => format!("{clean}.mp4"),
    }
}

/// 在檢視器裡串流播放；上次看到一半的從那裡接著看。同一排（資料夾、「繼續觀看」…）的其他影片
/// 用上一部 / 下一部切換，看完自動接下一支
fn play(app: &mut UiApp, d: &JfBrowse, item: &JfItem, row: &[JfItem]) {
    let Some(jf) = app.jellyfin() else { return };
    if item.duration <= 0.0 {
        return app.toast(tr!("Jellyfin 還沒分析好這支影片（長度不明），等一下再試", "Jellyfin hasn't finished analyzing this video yet. Try again shortly"), true);
    }
    let list: Vec<&JfItem> = row.iter().filter(|i| i.kind == JfKind::Video && i.duration > 0.0).collect();
    if let Some(idx) = list.iter().position(|i| i.id == item.id).filter(|_| list.len() > 1) {
        let queue = list.iter().map(|i| (entry_of(&jf, i), remote_of(&jf, d, i))).collect();
        let start = resume_at(item);
        viewer::open_remote_list(app, queue, idx, start);
        if let Some(t) = start {
            app.toast(trf!("從上次看到的 {} 接著看（拖曳進度列可以從頭看）", "Resuming from {} (drag the progress bar to start over)", clock(t)), false);
        }
        return;
    }
    let remote = remote_of(&jf, d, item);
    let entry = entry_of(&jf, item);
    match resume_at(item) {
        Some(t) => {
            viewer::open_remote(app, entry, remote, Some(t));
            app.toast(trf!("從上次看到的 {} 接著看（拖曳進度列可以從頭看）", "Resuming from {} (drag the progress bar to start over)", clock(t)), false);
        }
        None => viewer::open_remote(app, entry, remote, None),
    }
}

/// 看到一半（不是剛開始、也不是快看完）：從那裡接著看
fn resume_at(item: &JfItem) -> Option<f64> {
    (item.resume > 5.0 && item.resume < item.duration - 10.0).then_some(item.resume)
}

/// 串流用的字幕、看到哪裡記在誰、預覽縮圖
fn remote_of(jf: &Jellyfin, d: &JfBrowse, item: &JfItem) -> viewer::Remote {
    let mut r = viewer::Remote::new(&item.id, d.user.as_deref().unwrap_or(""), item.subs.iter().map(|s| jf.subtitle_url(&item.id, s.index)).collect(), &item.file_name);
    r.resume = item.resume;
    r.trickplay = item.trickplay.clone();
    r.favorite = item.favorite;
    r
}

/// 檢視器要的影片資訊（串流網址、長度、章節）
fn entry_of(jf: &Jellyfin, item: &JfItem) -> LibraryEntry {
    // 檢視器依副檔名判斷是不是影片：名稱用 Jellyfin 的標題加上原檔的副檔名
    let ext = item.file_name.rsplit_once('.').map(|(_, e)| e.to_string()).unwrap_or_else(|| "mp4".into());
    let mtime = chrono::NaiveDate::parse_from_str(&item.date, "%Y-%m-%d").ok().and_then(|d| d.and_hms_opt(12, 0, 0)).map_or(0.0, |t| t.and_utc().timestamp_millis() as f64);
    let media = MediaInfo {
        path: jf.stream_url(&item.id),
        name: format!("{}.{ext}", item.name),
        mtime,
        duration_sec: Some(item.duration),
        width: (item.width > 0).then_some(item.width),
        height: (item.height > 0).then_some(item.height),
        fps: Some(item.fps),
        has_audio: Some(item.has_audio),
        chapters: item.chapters.iter().filter(|(_, t)| !t.is_empty()).map(|(start, title)| Chapter { start: *start, title: title.clone() }).collect(),
        ..Default::default()
    };
    LibraryEntry { media, exports: vec![], hits: vec![] }
}

/// 在程式裡看了一段：瀏覽清單上的進度條、「看過」跟著更新
pub fn note_position(app: &mut UiApp, id: &str, at: f64, played: bool) {
    let Some(d) = app.jf_browse.as_mut() else { return };
    d.each_mut(id, |it| {
        it.played |= played;
        it.resume = if played { 0.0 } else { at };
    });
    let top = d.at_top();
    let item = d.items.iter().flatten().chain(d.latest.iter()).find(|i| i.id == id).cloned();
    if top {
        d.resume.retain(|i| i.id != id);
        if let Some(it) = item.filter(|_| !played && at > 5.0) {
            d.resume.insert(0, it);
        }
    }
}

/// 長度：1:02:03、2:03
fn clock(sec: f64) -> String {
    let s = sec.round() as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// 一格一格畫的時候按到了什麼
#[derive(Default)]
struct Picked {
    open: Option<JfItem>,
    /// 點開的那支所在的那一排（上一部 / 下一部、接著播）
    row: Vec<JfItem>,
    edit: Option<JfItem>,
    delete: Option<JfItem>,
    download: Option<JfItem>,
    toggle: Option<JfItem>,
    /// Shift＋按一下：（點的那一支, 它所在的那一排）
    range: Option<(JfItem, Vec<JfItem>)>,
    posters: Vec<String>,
    /// 右鍵或「加進…」：把這些影片加進合集、播放清單
    add_to: Option<AddTo>,
    /// 右鍵或下面的按鈕：從目前這個合集、播放清單移除這些（影片本身不刪）
    remove_here: Option<Vec<JfItem>>,
    /// 右鍵或下面的按鈕：幫這些影片產生字幕
    subs: Option<Vec<JfItem>>,
    /// 點了哪一個（鍵盤從那裡接著移動）
    cursor: Option<String>,
    /// 清單檢視點了欄位標題：換排序
    sort: Option<JfSort>,
    /// 捲到鍵盤移到的那一個了
    scrolled: bool,
    /// 縮圖一排幾個
    cols: Option<usize>,
    /// 滑鼠停在哪一支影片的縮圖上（滑過預覽、停住就播放）
    hover: Option<(JfItem, Rect)>,
}

/// 加進哪個合集、播放清單：to 是（編號, 名稱），None 是新建一個（先問名稱）
struct AddTo {
    kind: JfList,
    to: Option<(String, String)>,
    items: Vec<JfItem>,
}

/// 「加進…」選單的內容：合集、播放清單（後面寫幾支），各自最下面是新建；items 是要加進去的
fn add_menu(ui: &mut egui::Ui, d: &JfBrowse, items: &[JfItem], out: &mut Option<AddTo>) {
    let p = theme::pal(ui);
    ui.set_min_width(220.0);
    for (kind, title, lists, new) in [
        (JfList::Collection, tr!("加進合集", "Add to collection"), &d.collections, tr!("＋ 新合集…", "＋ New collection…")),
        (JfList::Playlist, tr!("加進播放清單", "Add to playlist"), &d.playlists, tr!("＋ 新播放清單…", "＋ New playlist…")),
    ] {
        ui.label(RichText::new(title).font(theme::font(12.0)).color(p.muted));
        for l in lists.iter() {
            let label = if l.children > 0 { format!("{}　{}", l.name, l.children) } else { l.name.clone() };
            if ui.button(label).clicked() {
                *out = Some(AddTo { kind, to: Some((l.id.clone(), l.name.clone())), items: items.to_vec() });
                ui.close();
            }
        }
        if ui.button(RichText::new(new).color(p.accent)).clicked() {
            *out = Some(AddTo { kind, to: None, items: items.to_vec() });
            ui.close();
        }
        if kind == JfList::Collection {
            ui.separator();
        }
    }
}

/// 畫一組縮圖格（rows：最多幾列，None = 全部）
fn cells(ui: &mut egui::Ui, d: &JfBrowse, items: &[JfItem], rows: Option<usize>, out: &mut Picked) {
    let p = theme::pal(ui);
    let gap = 12.0;
    let cols = ((ui.available_width() + gap) / (CELL_W[(d.grid as usize).min(2)] + gap)).floor().max(1.0) as usize;
    let cell_w = (ui.available_width() - gap * (cols as f32 - 1.0)) / cols as f32;
    let img_h = (cell_w * 9.0 / 16.0).round();
    let cell_h = img_h + 46.0;
    let clip = ui.clip_rect();
    let picking = !d.selected.is_empty();
    let (ctrl, shift) = ui.input(|i| (i.modifiers.command || i.modifiers.ctrl, i.modifiers.shift));
    if rows.is_none() {
        out.cols = Some(cols);
    }
    for row in items.chunks(cols).take(rows.unwrap_or(usize::MAX)) {
        let (row_rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), cell_h), Sense::hover());
        // 看不到的那一排不畫（捲過上千支時每一格都畫全部會很慢）；鍵盤移到看不到的那一支時照樣捲過去
        if !clip.intersects(row_rect) {
            if d.scroll_to_cursor && rows.is_none() && row.iter().any(|it| d.cursor.as_deref() == Some(it.id.as_str())) {
                ui.scroll_to_rect(row_rect, None);
            }
            continue;
        }
        for (i, it) in row.iter().enumerate() {
            let cell = Rect::from_min_size(pos2(row_rect.min.x + i as f32 * (cell_w + gap), row_rect.min.y), vec2(cell_w, cell_h));
            let resp = ui.interact(cell, ui.id().with(("jf-cell", &it.id)), Sense::click());
            let img = Rect::from_min_size(cell.min, vec2(cell_w, img_h));
            let video = it.kind == JfKind::Video;
            let on = d.selected.contains_key(&it.id);
            let painter = ui.painter();
            painter.rect_filled(img, 8.0, p.surface2);
            match d.posters.get(&it.id) {
                Some(Poster::Ready(tex)) => {
                    theme::paint_thumb_cover(painter, img, tex);
                    seen(&POSTER_SEEN, &it.id);
                }
                None if it.has_image && clip.intersects(cell) => out.posters.push(it.id.clone()),
                _ => {}
            }
            if !matches!(d.posters.get(&it.id), Some(Poster::Ready(_))) {
                let icon = if video {
                    Icon::Video
                } else if it.id == PLAYLISTS || it.id.starts_with(PL) {
                    Icon::List
                } else {
                    Icon::Folder
                };
                theme::paint_icon(painter, Rect::from_center_size(img.center(), vec2(32.0, 32.0)), icon, p.muted);
            }
            // 影片：滑鼠移上去時左上角勾選、左下角刪除和下載
            let check = Rect::from_min_size(img.min + vec2(6.0, 6.0), vec2(22.0, 22.0));
            let trash = Rect::from_min_size(pos2(img.min.x + 6.0, img.max.y - 30.0), vec2(24.0, 24.0));
            let down = trash.translate(vec2(30.0, 0.0));
            let edit = down.translate(vec2(30.0, 0.0));
            let inside = ui.rect_contains_pointer(cell);
            let hovering = resp.hovered() || inside && [check, trash, down, edit].iter().any(|r| ui.rect_contains_pointer(*r));
            let mut used = false;
            // 滑過預覽、停住就播放（在縮圖上、不在角落的按鈕上）
            let on_buttons = [check, trash, down, edit].iter().any(|r| ui.rect_contains_pointer(*r));
            if video && ui.rect_contains_pointer(img) && !resp.dragged() {
                if on_buttons {
                    super::hover_play::keep(ui.ctx(), &it.id);
                } else {
                    out.hover = Some((it.clone(), img));
                }
            }
            let playing = video && super::hover_play::paint(ui.painter(), &it.id, img, 8);
            // 滑過預覽：那個時間點的畫面直接畫在這張縮圖上（按鈕、標籤照樣在上面）
            let scrubbing = video && !playing && ui.rect_contains_pointer(img) && !resp.dragged() && super::thumbs::paint_scrub(ui.painter(), &it.id, img, 8);
            if video && (hovering || on || picking) {
                let r = ui.interact(check, ui.id().with(("jf-check", &it.id)), Sense::click()).on_hover_text(tr!("勾選（一次下載、刪除好幾支）", "Select (to download or delete several at once)"));
                let painter = ui.painter();
                painter.rect_filled(check, 5.0, if on { p.accent } else { egui::Color32::from_black_alpha(150) });
                painter.rect_stroke(check, 5.0, egui::Stroke::new(1.5, egui::Color32::WHITE), egui::StrokeKind::Inside);
                if on {
                    theme::paint_icon(painter, check.shrink(4.0), Icon::Check, egui::Color32::WHITE);
                }
                // 勾選框也可以 Shift＋按一下：從上次勾的選到這一支
                if r.clicked() {
                    if shift {
                        out.range = Some((it.clone(), items.to_vec()));
                    } else {
                        out.toggle = Some(it.clone());
                    }
                    used = true;
                }
            }
            if video && hovering {
                let tr_resp = ui.interact(trash, ui.id().with(("jf-del", &it.id)), Sense::click()).on_hover_text(tr!("從 Jellyfin 刪除", "Delete from Jellyfin"));
                let dl_resp =
                    ui.interact(down, ui.id().with(("jf-down", &it.id)), Sense::click()).on_hover_text(tr!("下載到這台電腦（下載回來就能剪輯）", "Download to this computer (so you can edit it)"));
                let ed_resp =
                    ui.interact(edit, ui.id().with(("jf-edit", &it.id)), Sense::click()).on_hover_text(tr!("影片資訊：改標題、人物、標籤、年份、封面", "Details: title, people, tags, year, cover"));
                let painter = ui.painter();
                painter.rect_filled(edit, 6.0, if ed_resp.hovered() { p.accent } else { egui::Color32::from_black_alpha(160) });
                theme::paint_icon(painter, edit.shrink(4.0), Icon::Edit, egui::Color32::WHITE);
                if ed_resp.clicked() {
                    out.edit = Some(it.clone());
                    used = true;
                }
                painter.rect_filled(trash, 6.0, if tr_resp.hovered() { p.rec } else { egui::Color32::from_black_alpha(160) });
                theme::paint_icon(painter, trash.shrink(4.0), Icon::Trash, egui::Color32::WHITE);
                painter.rect_filled(down, 6.0, if dl_resp.hovered() { p.accent } else { egui::Color32::from_black_alpha(160) });
                theme::paint_icon(painter, down.shrink(4.0), Icon::Download, egui::Color32::WHITE);
                if tr_resp.clicked() {
                    out.delete = Some(it.clone());
                    used = true;
                }
                if dl_resp.clicked() {
                    out.download = Some(it.clone());
                    used = true;
                }
            }
            let painter = ui.painter();
            if hovering || on {
                painter.rect_stroke(img, 8.0, egui::Stroke::new(2.0, p.accent), egui::StrokeKind::Inside);
            }
            // 鍵盤移到的：外面一圈虛一點的框
            if rows.is_none() && d.cursor.as_deref() == Some(it.id.as_str()) {
                painter.rect_stroke(cell.expand(3.0), 10.0, egui::Stroke::new(2.0, p.accent.gamma_multiply(0.6)), egui::StrokeKind::Outside);
                if d.scroll_to_cursor {
                    ui.scroll_to_rect(cell, None);
                    out.scrolled = true;
                }
            }
            if hovering && video && !picking && !playing && !scrubbing {
                theme::paint_icon(painter, Rect::from_center_size(img.center(), vec2(40.0, 40.0)), Icon::Play, egui::Color32::WHITE);
            }
            // 看到一半：縮圖下緣的進度條
            if video && it.resume > 0.0 && it.duration > 0.0 {
                let bar = Rect::from_min_max(pos2(img.min.x + 4.0, img.max.y - 5.0), pos2(img.max.x - 4.0, img.max.y - 2.0));
                painter.rect_filled(bar, 1.5, egui::Color32::from_black_alpha(140));
                let frac = (it.resume / it.duration).clamp(0.0, 1.0) as f32;
                painter.rect_filled(Rect::from_min_max(bar.min, pos2(bar.min.x + bar.width() * frac, bar.max.y)), 1.5, p.accent);
            }
            // 看過的：右上角綠色「看過」
            if it.played {
                let t = tr!("看過", "Watched");
                let g = painter.layout_no_wrap(t.to_string(), theme::font_bold(12.0), p.ok);
                let r = Rect::from_min_size(pos2(img.max.x - g.size().x - 14.0, img.min.y + 6.0), g.size() + vec2(8.0, 4.0));
                painter.rect_filled(r, 6.0, p.ok_soft);
                painter.galley(r.min + vec2(4.0, 2.0), g, p.ok);
            }
            // 已經有字幕的：右下角「CC」
            if video && !it.subs.is_empty() {
                let g = painter.layout_no_wrap("CC".to_string(), theme::font_bold(11.0), egui::Color32::WHITE);
                let r = Rect::from_min_size(pos2(img.max.x - g.size().x - 12.0, img.max.y - g.size().y - 10.0), g.size() + vec2(6.0, 2.0));
                painter.rect_filled(r, 3.0, egui::Color32::from_black_alpha(170));
                painter.galley(r.min + vec2(3.0, 1.0), g, egui::Color32::WHITE);
            }
            let name = painter.layout(it.name.clone(), theme::font_bold(12.5), p.text, cell_w);
            let name_rect = Rect::from_min_size(pos2(cell.min.x, img.max.y + 4.0), vec2(cell_w, 18.0));
            painter.with_clip_rect(name_rect).galley(name_rect.min, name, p.text);
            let sub = match it.kind {
                JfKind::Folder if it.children > 0 => trf!("{} 個", "{} items", it.children),
                JfKind::Folder => String::new(),
                JfKind::Video => [it.date.clone(), if it.duration > 0.0 { clock(it.duration) } else { String::new() }].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · "),
            };
            painter.text(pos2(cell.min.x, img.max.y + 24.0), Align2::LEFT_TOP, sub, theme::font(12.0), p.muted);
            let resp = resp.on_hover_text(&it.name);
            // 右鍵：播放、影片資訊、加進合集或播放清單、從這裡移除、下載、刪除（勾選的影片按右鍵是對全部勾選的）
            if video {
                resp.context_menu(|ui| item_menu(ui, d, it, items, on, out));
            }
            if resp.clicked() && !used {
                out.cursor = Some(it.id.clone());
                // Shift：從上次勾的選到這一支；已經在勾選、或按著 Ctrl：點影片是勾選；不然是播放（資料夾是進去）
                if video && shift {
                    out.range = Some((it.clone(), items.to_vec()));
                } else if video && (picking || ctrl) {
                    out.toggle = Some(it.clone());
                } else {
                    out.open = Some(it.clone());
                    out.row = items.to_vec();
                }
            }
        }
        ui.add_space(gap);
    }
}

/// 影片的右鍵選單：播放、影片資訊、加進合集或播放清單、從這裡移除、產生字幕、下載、刪除（on：這支有勾選時，對全部勾選的）
fn item_menu(ui: &mut egui::Ui, d: &JfBrowse, it: &JfItem, items: &[JfItem], on: bool, out: &mut Picked) {
    let p = theme::pal(ui);
    let targets: Vec<JfItem> = if on { d.selected.values().cloned().collect() } else { vec![it.clone()] };
    if targets.len() == 1 && ui.button(tr!("播放", "Play")).clicked() {
        out.open = Some(it.clone());
        out.row = items.to_vec();
        ui.close();
    }
    if targets.len() == 1 && ui.button(tr!("影片資訊…", "Details…")).clicked() {
        out.edit = Some(it.clone());
        ui.close();
    }
    let n = targets.len();
    let add = if n > 1 { trf!("把 {n} 支加進…", "Add {n} to…") } else { tr!("加進…", "Add to…").to_string() };
    ui.menu_button(add, |ui| add_menu(ui, d, &targets, &mut out.add_to));
    if let Some((kind, _)) = d.path.last().and_then(|(id, _)| list_at(id)) {
        let here = if kind == JfList::Collection { tr!("從這個合集移除", "Remove from this collection") } else { tr!("從這個播放清單移除", "Remove from this playlist") };
        if ui.button(here).clicked() {
            out.remove_here = Some(targets.clone());
            ui.close();
        }
    }
    let subs = if n > 1 { trf!("幫 {n} 支產生字幕", "Make subtitles for {n}") } else { tr!("產生字幕", "Make subtitles").to_string() };
    if ui
        .button(subs)
        .on_hover_text(tr!("用這台電腦的語音辨識做字幕，傳回 Jellyfin（電視、手機上看也有字幕）", "Recognize speech on this computer and add the subtitles to Jellyfin (shown on TVs and phones too)"))
        .clicked()
    {
        out.subs = Some(targets.clone());
        ui.close();
    }
    if n == 1 {
        ui.separator();
        if ui.button(tr!("下載到這台電腦", "Download to this computer")).clicked() {
            out.download = Some(it.clone());
            ui.close();
        }
        if ui.button(RichText::new(tr!("從 Jellyfin 刪除…", "Delete from Jellyfin…")).color(p.err)).clicked() {
            out.delete = Some(it.clone());
            ui.close();
        }
    }
}

/// 清單檢視的一欄：（寬度, 標題, 點標題時換的排序（第一次, 再按一次）, 靠左或靠右）
type Column<'a> = (f32, &'a str, Option<(JfSort, JfSort)>, Align);

/// 清單檢視：一列一個（名稱、長度、大小、日期、人物、標籤），點欄位標題換排序；按一下和縮圖一樣（播放、進資料夾；
/// Ctrl 勾選、Shift 選一段），右鍵同一個選單
fn rows(ui: &mut egui::Ui, d: &JfBrowse, items: &[JfItem], out: &mut Picked) {
    let p = theme::pal(ui);
    let w = ui.available_width();
    // 窄的時候不列人物、標籤
    let wide = w > 860.0;
    let (cw, lw, sw, dw, tw) = (30.0, 70.0, 84.0, 96.0, if wide { 170.0 } else { 0.0 });
    let nw = (w - cw - lw - sw - dw - tw * 2.0).max(150.0);
    let cols: Vec<Column> = [
        (cw, "", None, Align::Min),
        (nw, tr!("名稱", "Name"), Some((JfSort::Name, JfSort::NameDesc)), Align::Min),
        (lw, tr!("長度", "Length"), Some((JfSort::Longest, JfSort::Shortest)), Align::Max),
        (sw, tr!("大小", "Size"), None, Align::Max),
        (dw, tr!("日期", "Date"), Some((JfSort::Newest, JfSort::Oldest)), Align::Max),
        (tw, tr!("人物", "People"), None, Align::Min),
        (tw, tr!("標籤", "Tags"), None, Align::Min),
    ]
    .into_iter()
    .filter(|c| c.0 > 0.0)
    .collect();
    // 一格裡的字（太長截短；回傳有沒有截掉）
    let cell_text = |ui: &egui::Ui, rect: Rect, text: &str, align: Align, color: egui::Color32, bold: bool| -> bool {
        let font = if bold { theme::font_bold(12.5) } else { theme::font(12.5) };
        let inner = rect.shrink2(vec2(6.0, 0.0));
        let mut job = egui::text::LayoutJob::simple_singleline(text.to_string(), font, color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(inner.width());
        let g = ui.painter().layout_job(job);
        let x = if align == Align::Max { inner.max.x - g.size().x } else { inner.min.x };
        let cut = g.elided;
        ui.painter().galley(pos2(x, rect.center().y - g.size().y / 2.0), g, color);
        cut
    };
    // 標題列
    let (head, _) = ui.allocate_exact_size(vec2(w, 28.0), Sense::hover());
    let mut x = head.min.x;
    for (cwid, title, sorts, align) in &cols {
        let r = Rect::from_min_size(pos2(x, head.min.y), vec2(*cwid, head.height()));
        x += cwid;
        let Some((a, b)) = sorts else {
            cell_text(ui, r, title, *align, p.muted, false);
            continue;
        };
        let now = d.sort == *a || d.sort == *b;
        let arrow = if d.sort == *a {
            " ↓"
        } else if d.sort == *b {
            " ↑"
        } else {
            ""
        };
        let resp = ui.interact(r, ui.id().with(("jf-head", *title)), Sense::click()).on_hover_text(tr!("按一下依這一欄排序（再按一下倒過來）", "Click to sort by this column (again to reverse)"));
        if resp.hovered() {
            ui.painter().rect_filled(r, 4.0, p.surface2);
        }
        cell_text(ui, r, &format!("{title}{arrow}"), *align, if now { p.text } else { p.muted }, now);
        if resp.clicked() {
            out.sort = Some(if d.sort == *a { *b } else { *a });
        }
    }
    ui.painter().hline(head.x_range(), head.max.y, egui::Stroke::new(1.0, p.border));
    let picking = !d.selected.is_empty();
    let (ctrl, shift) = ui.input(|i| (i.modifiers.command || i.modifiers.ctrl, i.modifiers.shift));
    let clip = ui.clip_rect();
    for it in items {
        let (row, _) = ui.allocate_exact_size(vec2(w, 32.0), Sense::hover());
        if !clip.intersects(row) {
            continue;
        }
        let video = it.kind == JfKind::Video;
        let on = d.selected.contains_key(&it.id);
        let resp = ui.interact(row, ui.id().with(("jf-row", &it.id)), Sense::click());
        let fill = if on {
            p.accent_soft
        } else if resp.hovered() {
            p.surface2
        } else {
            egui::Color32::TRANSPARENT
        };
        ui.painter().rect_filled(row, 4.0, fill);
        if d.cursor.as_deref() == Some(it.id.as_str()) {
            ui.painter().rect_stroke(row, 4.0, egui::Stroke::new(1.5, p.accent.gamma_multiply(0.7)), egui::StrokeKind::Inside);
            if d.scroll_to_cursor {
                ui.scroll_to_rect(row, None);
                out.scrolled = true;
            }
        }
        let mut x = row.min.x;
        let mut used = false;
        // 人物、標籤太長被截掉、滑鼠在那一格上：提示完整的（不然提示名稱）
        let mut tip: Option<String> = None;
        let pointer = ui.ctx().pointer_hover_pos();
        for (cwid, title, _, align) in &cols {
            let r = Rect::from_min_size(pos2(x, row.min.y), vec2(*cwid, row.height()));
            x += cwid;
            match *title {
                "" => {
                    let icon_r = Rect::from_center_size(r.center(), vec2(16.0, 16.0));
                    if video {
                        let cr = ui.interact(icon_r.expand(4.0), ui.id().with(("jf-row-check", &it.id)), Sense::click());
                        ui.painter().rect_filled(icon_r, 3.0, if on { p.accent } else { p.surface });
                        ui.painter().rect_stroke(icon_r, 3.0, egui::Stroke::new(1.2, if on { p.accent } else { p.border }), egui::StrokeKind::Inside);
                        if on {
                            theme::paint_icon(ui.painter(), icon_r.shrink(2.0), Icon::Check, egui::Color32::WHITE);
                        }
                        if cr.clicked() {
                            if shift {
                                out.range = Some((it.clone(), items.to_vec()));
                            } else {
                                out.toggle = Some(it.clone());
                            }
                            out.cursor = Some(it.id.clone());
                            used = true;
                        }
                    } else {
                        let icon = if it.id == PLAYLISTS || it.id.starts_with(PL) { Icon::List } else { Icon::Folder };
                        theme::paint_icon(ui.painter(), icon_r, icon, p.muted);
                    }
                }
                t if t == tr!("名稱", "Name") => {
                    let mut name = it.name.clone();
                    if it.played {
                        name = trf!("{name}　✓ 看過", "{name}  ✓ watched");
                    }
                    cell_text(ui, r, &name, *align, p.text, !video);
                    if !it.subs.is_empty() {
                        // 名稱後面的 CC 不另外量寬度：放在這一欄的最右邊
                        let g = ui.painter().layout_no_wrap("CC".to_string(), theme::font_bold(11.0), p.muted);
                        let b = Rect::from_min_size(pos2(r.max.x - g.size().x - 12.0, r.center().y - g.size().y / 2.0 - 1.0), g.size() + vec2(6.0, 2.0));
                        ui.painter().rect_stroke(b, 3.0, egui::Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
                        ui.painter().galley(b.min + vec2(3.0, 1.0), g, p.muted);
                    }
                }
                t if t == tr!("長度", "Length") => {
                    let text = match it.kind {
                        JfKind::Folder if it.children > 0 => trf!("{} 個", "{} items", it.children),
                        JfKind::Folder => String::new(),
                        JfKind::Video if it.duration > 0.0 => clock(it.duration),
                        JfKind::Video => String::new(),
                    };
                    cell_text(ui, r, &text, *align, p.muted, false);
                }
                t if t == tr!("大小", "Size") => {
                    let text = if it.size > 0 { screenrecorder_core::format::format_bytes(it.size) } else { String::new() };
                    cell_text(ui, r, &text, *align, p.muted, false);
                }
                t if t == tr!("日期", "Date") => {
                    cell_text(ui, r, &it.date, *align, p.muted, false);
                }
                t => {
                    let text = if t == tr!("人物", "People") { it.people.join(tr!("、", ", ")) } else { it.tags.join(tr!("、", ", ")) };
                    if cell_text(ui, r, &text, *align, p.text, false) && pointer.is_some_and(|q| r.contains(q)) {
                        tip = Some(text);
                    }
                }
            }
        }
        let resp = resp.on_hover_text(tip.as_deref().unwrap_or(&it.name));
        if video {
            resp.context_menu(|ui| item_menu(ui, d, it, items, on, out));
        }
        if resp.clicked() && !used {
            out.cursor = Some(it.id.clone());
            if video && shift {
                out.range = Some((it.clone(), items.to_vec()));
            } else if video && (picking || ctrl) {
                out.toggle = Some(it.clone());
            } else {
                out.open = Some(it.clone());
                out.row = items.to_vec();
            }
        }
    }
    ui.add_space(8.0);
}

/// 鍵盤（沒有在打字、沒有開啟的選單或對話框時）：方向鍵移動、Enter 開啟、空白鍵勾選、Ctrl+A 勾選全部、
/// Delete 刪除勾選的（沒勾選時是移到的那一支）、Backspace 回上一層
fn keyboard(ui: &egui::Ui, d: &mut JfBrowse, out: &mut Panel) {
    if ui.ctx().egui_wants_keyboard_input() || egui::Popup::is_any_open(ui.ctx()) {
        return;
    }
    let (left, right, up, down, enter, space, del, back, all) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::ArrowLeft),
            i.key_pressed(egui::Key::ArrowRight),
            i.key_pressed(egui::Key::ArrowUp),
            i.key_pressed(egui::Key::ArrowDown),
            i.key_pressed(egui::Key::Enter),
            i.key_pressed(egui::Key::Space),
            i.key_pressed(egui::Key::Delete),
            i.key_pressed(egui::Key::Backspace),
            i.modifiers.command && i.key_pressed(egui::Key::A),
        )
    });
    // 沒按鍵（幾乎每一格都是）：不用複製整個清單
    if !(left || right || up || down || enter || space || del || back || all) {
        return;
    }
    let Some(items) = d.items.clone().filter(|i| !i.is_empty()) else { return };
    let step = if d.list_view && !d.at_top() { 1 } else { d.cols.max(1) as isize as usize };
    let at = d.cursor.as_ref().and_then(|c| items.iter().position(|i| &i.id == c));
    let mv: isize = if left && !(d.list_view && !d.at_top()) {
        -1
    } else if right && !(d.list_view && !d.at_top()) {
        1
    } else if up {
        -(step as isize)
    } else if down {
        step as isize
    } else {
        0
    };
    if mv != 0 {
        let next = match at {
            None => 0,
            Some(i) => (i as isize + mv).clamp(0, items.len() as isize - 1) as usize,
        };
        d.cursor = Some(items[next].id.clone());
        d.scroll_to_cursor = true;
        // 快到最後面時讀下一批
        if next + step * 2 >= items.len() {
            out.more = true;
        }
    }
    let cur = at.map(|i| items[i].clone());
    if enter {
        if let Some(it) = cur.clone() {
            out.picked.open = Some(it);
            out.picked.row = items.clone();
        }
    }
    if space {
        if let Some(it) = cur.clone().filter(|i| i.kind == JfKind::Video) {
            out.picked.toggle = Some(it);
        }
    }
    if all {
        for it in items.iter().filter(|i| i.kind == JfKind::Video) {
            d.selected.insert(it.id.clone(), it.clone());
        }
    }
    if del {
        if !d.selected.is_empty() {
            out.bulk_delete = true;
        } else if let Some(it) = cur.filter(|i| i.kind == JfKind::Video) {
            out.picked.delete = Some(it);
        }
    }
    if back && !d.path.is_empty() {
        d.path.pop();
        d.page = 0;
        d.cursor = None;
        out.reload = true;
    }
}

/// 「依這些找」的改變
enum FacetChange {
    None,
    /// 勾或取消勾一個
    Toggle(JfFacet),
    /// 清除一類（0 人物、1 標籤、2 年份）
    ClearKind(u8),
    ClearAll,
}

/// 條件是哪一類：0 人物、1 標籤、2 年份
fn kind_of(f: &JfFacet) -> u8 {
    match f {
        JfFacet::Person(..) | JfFacet::NoPeople => 0,
        JfFacet::Tag(_) | JfFacet::NoTags => 1,
        JfFacet::Year(_) => 2,
    }
}

fn facet_name(f: &JfFacet) -> String {
    match f {
        JfFacet::Person(_, n) => n.clone(),
        JfFacet::Tag(t) => t.clone(),
        JfFacet::Year(y) => y.to_string(),
        JfFacet::NoPeople => tr!("沒有人物", "No people").into(),
        JfFacet::NoTags => tr!("沒有標籤", "No tags").into(),
    }
}

/// 「沒有人物」「沒有標籤」（和同一類的其他條件不能一起勾：沒有人物又有小美不會有影片）
fn is_none_facet(f: &JfFacet) -> bool {
    matches!(f, JfFacet::NoPeople | JfFacet::NoTags)
}

/// 套用「依這些找」的改變：找目前資料夾底下（含子資料夾；最上層時整個伺服器）的影片，搜尋清掉
fn apply_facet(d: &mut JfBrowse, change: FacetChange, out: &mut Panel) {
    match change {
        FacetChange::None => return,
        FacetChange::Toggle(f) => match d.facet.iter().position(|x| *x == f) {
            Some(i) => {
                d.facet.remove(i);
            }
            None => {
                // 「沒有人物」和人物（「沒有標籤」和標籤）只留剛勾的那一邊
                let (k, none) = (kind_of(&f), is_none_facet(&f));
                d.facet.retain(|x| kind_of(x) != k || is_none_facet(x) == none);
                d.facet.push(f)
            }
        },
        FacetChange::ClearKind(k) => d.facet.retain(|f| kind_of(f) != k),
        FacetChange::ClearAll => d.facet.clear(),
    }
    d.search.clear();
    d.search_at = None;
    d.page = 0;
    out.reload = true;
}

/// 「人物 ⌄」「標籤 ⌄」「年份 ⌄」其中一個：按鈕上的藍色數字是勾了幾個；開啟是搜尋框和勾選清單（後面寫用在幾支影片），
/// 勾了馬上套用、清單不關（可以連續勾），最下面可以清除這一類。rows：（名稱, 幾支, 條件）
#[allow(clippy::too_many_arguments)]
fn facet_menu(
    ui: &mut egui::Ui,
    kind: u8,
    empty: &str,
    count: usize,
    rows: impl FnOnce() -> Vec<(String, usize, JfFacet)>,
    chosen: &[JfFacet],
    search: &mut String,
    change: &mut FacetChange,
    faces: &HashMap<String, Poster>,
    want_faces: &mut Vec<String>,
) {
    let p = theme::pal(ui);
    let label = [tr!("人物", "People"), tr!("標籤", "Tags"), tr!("年份", "Year")][kind as usize];
    let n = chosen.iter().filter(|f| kind_of(f) == kind).count();
    // 寬度固定（先留好藍色數字的位置）：勾了只是數字出現，按鈕不變寬、整排不會移動
    let w = Btn::new(label).menu().badge(9).height(super::toolbar::H).width(ui);
    let b = Btn::new(label).menu().selected(n > 0).badge(n).min_width(w).height(super::toolbar::H).enabled(count > 0).tooltip(if count == 0 { empty } else { "" }).show(ui);
    egui::Popup::menu(&b).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        let rows = rows();
        // 寬度固定：清單在選單裡，靠右的東西不能讓它每一格越撐越寬
        const W: f32 = 300.0;
        ui.set_width(W);
        ui.set_max_width(W);
        let hint = [tr!("搜尋人物…", "Search people…"), tr!("搜尋標籤…", "Search tags…"), tr!("搜尋年份…", "Search years…")][kind as usize];
        let sr = ui.add(egui::TextEdit::singleline(search).hint_text(hint).desired_width(W).margin(egui::Margin { left: 4, right: 26, top: 2, bottom: 2 }));
        super::toolbar::clear_x(ui, &sr, search);
        ui.add_space(4.0);
        let q = jellyfin::name_key(search);
        egui::ScrollArea::vertical().id_salt(("jf-facet", kind)).max_height(300.0).show(ui, |ui| {
            let mut any = false;
            for (name, c, f) in rows.iter().filter(|(n, ..)| q.is_empty() || jellyfin::name_key(n).contains(&q)) {
                any = true;
                // 每一列固定大小：勾選框＋名稱、右邊的支數（畫上去，選單才不會越撐越寬）。
                // 人物那一列高一點：勾選框和名稱中間一個圓形頭像（沒有照片時是名字的第一個字）
                let person = match f {
                    JfFacet::Person(id, _) => Some(id),
                    _ => None,
                };
                let (row, row_resp) = ui.allocate_exact_size(vec2(W - 16.0, if person.is_some() { 40.0 } else { 26.0 }), if person.is_some() { Sense::click() } else { Sense::hover() });
                // 捲到看不到的列只佔位置不畫（人物、標籤有幾百個）
                if !ui.is_rect_visible(row) {
                    if is_none_facet(f) {
                        ui.separator();
                    }
                    continue;
                }
                let cw = if *c > 0 || is_none_facet(f) { 56.0 } else { 0.0 };
                let mut on = chosen.contains(f);
                let shown: String = if name.chars().count() > 24 { name.chars().take(23).chain(['…']).collect() } else { name.clone() };
                let r = if let Some(id) = person {
                    if row_resp.hovered() {
                        ui.painter().rect_filled(row, 4.0, p.surface2.gamma_multiply(0.6));
                    }
                    let cb = Rect::from_min_size(pos2(row.min.x + 2.0, row.center().y - 10.0), vec2(20.0, 20.0));
                    let mut r = ui.scope_builder(UiBuilder::new().max_rect(cb).layout(Layout::left_to_right(Align::Center)), |ui| theme::check(ui, &mut on, "")).inner;
                    let at = Rect::from_min_size(pos2(cb.max.x + 6.0, row.center().y - 16.0), vec2(32.0, 32.0));
                    let ar = ui.interact(at, egui::Id::new(("jf-facet-face", id)), Sense::click());
                    match faces.get(id) {
                        Some(Poster::Ready(tex)) => {
                            seen(&FACE_SEEN, id);
                            // 人物照片是直的：取上半部的正方形（臉通常在那裡），不要壓扁
                            let [w, h] = tex.size().map(|v| v.max(1) as f32);
                            let uv = if h > w {
                                let span = w / h;
                                let v0 = (0.4 - span / 2.0).clamp(0.0, 1.0 - span);
                                Rect::from_min_max(pos2(0.0, v0), pos2(1.0, v0 + span))
                            } else {
                                let span = h / w;
                                Rect::from_min_max(pos2(0.5 - span / 2.0, 0.0), pos2(0.5 + span / 2.0, 1.0))
                            };
                            egui::Image::new(tex).uv(uv).fit_to_exact_size(at.size()).corner_radius(16.0).paint_at(ui, at);
                            // 滑過頭像看大張的
                            let tex = tex.clone();
                            ar.clone().on_hover_ui(move |ui| {
                                let s = vec2(140.0, 140.0 * h / w);
                                ui.add(egui::Image::new(&tex).fit_to_exact_size(s).corner_radius(6.0));
                            });
                        }
                        other => {
                            if other.is_none() && ui.is_rect_visible(at) {
                                want_faces.push(id.clone());
                            }
                            ui.painter().circle_filled(at.center(), 16.0, p.surface2);
                            let first: String = name.chars().next().map(String::from).unwrap_or_default();
                            ui.painter().text(at.center(), Align2::CENTER_CENTER, first, theme::font_bold(14.0), p.muted);
                        }
                    }
                    let name_max = row.max.x - cw - at.max.x - 12.0;
                    let galley = ui.painter().layout(shown, theme::font(14.0), p.text, name_max);
                    ui.painter().galley(pos2(at.max.x + 8.0, row.center().y - galley.size().y / 2.0), galley, p.text);
                    if !r.changed() && (row_resp.clicked() || ar.clicked()) {
                        r.mark_changed();
                    }
                    r
                } else {
                    let cb = Rect::from_min_max(row.min, pos2(row.max.x - cw, row.max.y));
                    ui.scope_builder(UiBuilder::new().max_rect(cb).layout(Layout::left_to_right(Align::Center)), |ui| theme::check(ui, &mut on, shown)).inner
                };
                if r.changed() {
                    *change = FacetChange::Toggle(f.clone());
                }
                if *c > 0 || is_none_facet(f) {
                    ui.painter().text(pos2(row.max.x - 4.0, row.center().y), Align2::RIGHT_CENTER, trf!("{c} 支", "{c}"), theme::font(12.0), p.muted);
                }
                if is_none_facet(f) {
                    r.on_hover_text(tr!(
                        "還沒填的影片：勾了列出來，全選後按下面的「整理人物、標籤…」一次補好",
                        "Videos not filled in yet: check this to list them, select all, then use “Organize people & tags…” below"
                    ));
                    ui.separator();
                }
            }
            if !any {
                ui.label(RichText::new(tr!("沒有符合的", "No matches")).font(theme::font(12.5)).color(p.muted));
            }
        });
        ui.separator();
        let (row, _) = ui.allocate_exact_size(vec2(W, 28.0), Sense::hover());
        let note = if n > 0 {
            trf!("已勾 {n} 個・勾了馬上套用", "{n} checked · applies right away")
        } else {
            tr!("勾了馬上套用，可以勾好幾個", "Applies right away; check as many as you like").to_string()
        };
        ui.painter().text(pos2(row.min.x, row.center().y), Align2::LEFT_CENTER, note, theme::font(12.0), p.muted);
        let lower = label.to_lowercase();
        let clear = Btn::new(trf!("清除{label}", "Clear {lower}")).small().ghost().enabled(n > 0);
        let br = Rect::from_min_max(pos2(row.max.x - clear.width(ui), row.min.y), row.max);
        if ui.scope_builder(UiBuilder::new().max_rect(br), |ui| clear.show(ui)).inner.clicked() {
            *change = FacetChange::ClearKind(kind);
        }
    });
}

/// 條件（放在「媒體庫 ›」後面同一列）：每個條件一顆（按一下移除），同一類之間寫「或」、不同類之間寫「且」，最後是「全部清除」。
/// room 放不下時先放得下的，後面一顆「+N 個」開啟清單看其他的（一樣可以移除）
fn facet_chips(ui: &mut egui::Ui, chosen: &[JfFacet], room: f32, change: &mut FacetChange) {
    let p = theme::pal(ui);
    ui.spacing_mut().item_spacing.x = 6.0;
    let gap = 6.0;
    let ordered: Vec<&JfFacet> = (0..3u8).flat_map(|k| chosen.iter().filter(move |f| kind_of(f) == k)).collect();
    let text_w = |t: &str, f: egui::FontId| ui.painter().layout_no_wrap(t.to_string(), f, p.text).size().x;
    let clear = Btn::new(tr!("全部清除", "Clear all")).small().ghost().tooltip(tr!("移除所有條件，回到媒體庫", "Remove all conditions and go back to the libraries"));
    let clear_w = clear.width(ui) + gap;
    let more_w = Btn::new(trf!("+{} 個", "+{} more", 99)).small().ghost().width(ui) + gap;
    let sep = |a: &JfFacet, b: &JfFacet| if kind_of(a) == kind_of(b) { tr!("或", "or") } else { tr!("且", "and") };
    // 放得下幾個（至少一個）
    let (mut used, mut shown) = (0.0, 0);
    for (i, f) in ordered.iter().enumerate() {
        let sep_w = if i == 0 { 0.0 } else { text_w(sep(ordered[i - 1], f), theme::font_bold(12.5)) + gap };
        let w = sep_w + text_w(&facet_name(f), theme::font(12.5)) + 34.0 + gap;
        let reserve = clear_w + if i + 1 < ordered.len() { more_w } else { 0.0 };
        if shown > 0 && used + w + reserve > room {
            break;
        }
        used += w;
        shown += 1;
    }
    let kind_name = |f: &JfFacet| [tr!("人物", "Person"), tr!("標籤", "Tag"), tr!("年份", "Year")][kind_of(f) as usize];
    for (i, f) in ordered.iter().take(shown).enumerate() {
        if i > 0 {
            let s = sep(ordered[i - 1], f);
            let and = kind_of(ordered[i - 1]) != kind_of(f);
            ui.label(RichText::new(s).font(if and { theme::font_bold(12.5) } else { theme::font(12.0) }).color(if and { p.text } else { p.muted }));
        }
        let tip = trf!("{}：{}，按一下移除這個條件", "{}: {}. Click to remove this condition", kind_name(f), facet_name(f));
        if remove_chip(ui, &facet_name(f)).on_hover_text(tip).clicked() {
            *change = FacetChange::Toggle((*f).clone());
        }
    }
    let hidden = &ordered[shown..];
    if !hidden.is_empty() {
        let b = Btn::new(trf!("+{} 個", "+{} more", hidden.len())).small().ghost().tooltip(tr!("看其他的條件", "See the other conditions")).show(ui);
        egui::Popup::menu(&b).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            for f in hidden {
                ui.horizontal(|ui| {
                    ui.label(theme::muted(ui, kind_name(f)));
                    if remove_chip(ui, &facet_name(f)).on_hover_text(tr!("按一下移除這個條件", "Click to remove this condition")).clicked() {
                        *change = FacetChange::Toggle((*f).clone());
                    }
                });
            }
        });
    }
    if clear.show(ui).clicked() {
        *change = FacetChange::ClearAll;
    }
}

/// 藍色小標籤，後面有 ×：整顆按一下移除
fn remove_chip(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let p = theme::pal(ui);
    let g = ui.painter().layout_no_wrap(text.to_string(), theme::font(12.5), p.accent);
    let size = vec2(g.size().x + 10.0 + 6.0 + 10.0 + 8.0, 24.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let fill = if resp.hovered() { p.accent.gamma_multiply(0.22) } else { p.accent_soft };
    ui.painter().rect_filled(rect, 12.0, fill);
    ui.painter().galley_with_override_text_color(pos2(rect.min.x + 10.0, rect.center().y - g.size().y / 2.0), g, p.accent);
    theme::paint_icon(ui.painter(), Rect::from_center_size(pos2(rect.max.x - 13.0, rect.center().y), vec2(10.0, 10.0)), Icon::Close, p.accent);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// 目前在找什麼（例如「有小美或小明、2024 年的影片」）
fn facet_summary(chosen: &[JfFacet]) -> String {
    let names = |k: u8| chosen.iter().filter(|f| kind_of(f) == k && !is_none_facet(f)).map(facet_name).collect::<Vec<_>>();
    let or = tr!("或", " or ");
    let mut parts = vec![];
    if chosen.contains(&JfFacet::NoPeople) {
        parts.push(tr!("還沒設人物", "with no people").to_string());
    }
    if chosen.contains(&JfFacet::NoTags) {
        parts.push(tr!("還沒有標籤", "with no tags").to_string());
    }
    let people = names(0);
    if !people.is_empty() {
        parts.push(trf!("有{}", "with {}", people.join(or)));
    }
    let tags: Vec<String> = names(1).into_iter().map(|t| trf!("「{t}」", "“{t}”")).collect();
    if !tags.is_empty() {
        parts.push(trf!("標籤{}", "tagged {}", tags.join(or)));
    }
    let years = names(2);
    if !years.is_empty() {
        parts.push(trf!("{} 年", "from {}", years.join(or)));
    }
    trf!("{}的影片", "videos {}", parts.join(tr!("、", ", ")))
}

/// 區塊標題（「繼續觀看」等）
fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(2.0);
    ui.label(RichText::new(title).font(theme::font_bold(14.0)));
    ui.add_space(4.0);
}

/// 在檔案庫「Jellyfin」分頁上按了什麼（畫完之後才處理，處理時要用到 app）
#[derive(Default)]
pub struct Panel {
    reload: bool,
    /// 捲到快底了：讀下一批
    more: bool,
    picked: Picked,
    bulk_download: bool,
    bulk_delete: bool,
    /// 勾選的一起整理人物、標籤、年份
    bulk_organize: bool,
    /// 在合集、播放清單裡按「全部播放」
    play_all: bool,
    /// 按了產生字幕進度旁的「取消」
    cancel_subs: bool,
    /// 按了整理人物、標籤進度旁的「取消」
    cancel_batch: bool,
    /// 「依這些找」的人物清單看到了、還沒讀的人物照片
    faces: Vec<String>,
    find_dupes: bool,
}

impl JfBrowse {
    /// Esc：有勾選時先取消勾選（回傳 true），沒有才關掉檔案庫
    pub fn clear_selection(&mut self) -> bool {
        let had = !self.selected.is_empty();
        self.selected.clear();
        had
    }
}

/// 在縮圖上按住 Ctrl 轉滾輪：換縮圖大小（轉一下換一級）
fn grid_zoom(ui: &egui::Ui, d: &mut JfBrowse, area: Rect) {
    if !ui.rect_contains_pointer(area) || egui::Popup::is_any_open(ui.ctx()) {
        return;
    }
    let dy: f32 = ui.input(|i| {
        i.raw
            .events
            .iter()
            .filter_map(|e| match e {
                egui::Event::MouseWheel { delta, modifiers, .. } if modifiers.ctrl || modifiers.command => Some(delta.y),
                _ => None,
            })
            .sum()
    });
    if dy == 0.0 || d.wheel_at.is_some_and(|t| t.elapsed().as_millis() < 250) {
        return;
    }
    let next = if dy > 0.0 { d.grid.saturating_add(1).min(2) } else { d.grid.saturating_sub(1) };
    if next != d.grid {
        d.grid = next;
        d.wheel_at = Some(Instant::now());
    }
}

/// 檔案庫的「Jellyfin」分頁：上面搜尋、依人物 / 標籤 / 年份找、找重複；接著目前位置與排序；
/// 中間縮圖格；最下面勾選的操作與換頁（和錄影、截圖分頁同一個排法）
/// busy：背景正在整理好幾支影片時（改好幾支, 全部幾支），下面畫進度
/// subs：背景正在產生字幕時（第幾支, 全部幾支, 名稱, 百分比）
/// keys：可以用鍵盤（上面沒有其他對話框）
pub fn panel(ui: &mut egui::Ui, d: &mut JfBrowse, busy: Option<(usize, usize)>, subs: Option<(usize, usize, String, u32)>, keys: bool) -> Panel {
    let mut out = Panel::default();
    if keys {
        keyboard(ui, d, &mut out);
    }
    let p = theme::pal(ui);
    // 和錄影、截圖同一個順序：搜尋 → 篩選 → 排序 → 縮圖大小 / 清單 → Jellyfin 才有的（依人物、標籤、年份找，找重複的影片）
    let top = d.at_top();
    let here = d.path.last().map(|(id, _)| id.clone()).unwrap_or_default();
    let in_list = list_at(&here).map(|(k, _)| k);
    let lists_page = here == COLLECTIONS || here == PLAYLISTS;
    // 最上層、播放清單、「合集」「播放清單」那一層照固定的順序：不排序、不篩選
    let ordered = top || (d.searched.is_empty() && d.facet.is_empty() && (in_list == Some(JfList::Playlist) || lists_page));
    super::toolbar::bar(ui, egui::Id::new("jfbar"), |ui, part| match part {
        super::toolbar::Part::Search(w) => {
            let r = ui.add(super::toolbar::search_box(&mut d.search, tr!("搜尋 Jellyfin 上的影片", "Search videos on Jellyfin"), w));
            if r.changed() {
                d.search_at = Some(Instant::now());
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                d.search_at = Some(Instant::now() - SEARCH_WAIT);
            }
            if super::toolbar::clear_x(ui, &r, &mut d.search) {
                d.search_at = Some(Instant::now() - SEARCH_WAIT);
            }
        }
        // 篩選、排序：資料夾裡篩選列出底下符合的影片；搜尋、依條件找時從結果裡再篩
        super::toolbar::Part::Left => {
            let (sort, filter) = (d.sort, d.filter);
            if !ordered {
                let tip = if !d.facet.is_empty() {
                    tr!("符合條件的影片裡，只列出這一種", "Only list this kind among the videos that match")
                } else if !d.searched.is_empty() {
                    tr!("搜尋結果裡，只列出這一種", "Only list this kind among the search results")
                } else {
                    tr!("不是「全部」時，列出這個資料夾底下（含子資料夾）符合的影片", "Anything but “All” lists matching videos in this folder and its subfolders")
                };
                egui::ComboBox::from_id_salt("jf-filter")
                    .selected_text(d.filter.label())
                    .width(120.0)
                    .show_ui(ui, |ui| {
                        for f in JfFilter::ALL {
                            ui.selectable_value(&mut d.filter, f, f.label());
                        }
                    })
                    .response
                    .on_hover_text(tip);
                egui::ComboBox::from_id_salt("jf-sort").selected_text(d.sort.label()).width(130.0).show_ui(ui, |ui| {
                    for s in JfSort::ALL {
                        ui.selectable_value(&mut d.sort, s, s.label());
                    }
                });
            }
            if (sort, filter) != (d.sort, d.filter) {
                d.page = 0;
                out.reload = true;
            }
        }
        super::toolbar::Part::Right(compact) => {
            // 縮圖大小或清單（最上層只有縮圖）；縮圖也可以按住 Ctrl 轉滾輪調整
            let tip = tr!(
                "縮圖大小，或改成一列一支的清單。也可以在縮圖上按住 Ctrl 轉滾輪調整大小",
                "Thumbnail size, or a list with one video per row. You can also hold Ctrl and turn the mouse wheel over the thumbnails"
            );
            let mut view = if d.list_view && !top { None } else { Some(d.grid) };
            let mut opts = vec![(Some(0u8), tr!("小", "S")), (Some(1), tr!("中", "M")), (Some(2), tr!("大", "L"))];
            if !top {
                opts.push((None, tr!("清單", "List")));
            }
            if super::toolbar::view_switch(ui, &mut view, &opts, compact, tip) {
                match view {
                    Some(g) => {
                        d.grid = g;
                        d.list_view = false;
                    }
                    None => d.list_view = true,
                }
            }
            // 依人物、標籤、年份找（各一個勾選清單，勾了馬上套用、清單不關；按鈕上的藍色數字是勾了幾個）
            ui.add_space(10.0);
            let mut change = FacetChange::None;
            let why = |none: &str| match &d.facets_error {
                Some(e) => trf!("讀不到 Jellyfin 的標籤、年份：{e}", "Couldn't read tags and years from Jellyfin: {e}"),
                None => none.to_string(),
            };
            // 清單只在打開時才建（沒打開時每一格都把幾百個人物、標籤複製一遍很浪費）
            let years = || d.years.iter().map(|(y, c)| (y.to_string(), *c, JfFacet::Year(*y))).collect::<Vec<_>>();
            // 最上面一列是「（沒有標籤）」「（沒有人物）」：找還沒填的
            let none_row = |n: usize, f: JfFacet| (trf!("（{}）", "({})", facet_name(&f)), n, f);
            let tags = || std::iter::once(none_row(d.no_tags, JfFacet::NoTags)).chain(d.tags.iter().map(|(t, c)| (t.clone(), *c, JfFacet::Tag(t.clone())))).collect::<Vec<_>>();
            let people =
                || std::iter::once(none_row(d.no_people, JfFacet::NoPeople)).chain(d.persons.iter().map(|(id, n, c)| (n.clone(), *c, JfFacet::Person(id.clone(), n.clone())))).collect::<Vec<_>>();
            let mut search = std::mem::take(&mut d.facet_search);
            let [s0, s1, s2] = &mut search;
            facet_menu(ui, 0, tr!("影片還沒有設人物", "No videos have people yet"), d.persons.len() + 1, people, &d.facet, s0, &mut change, &d.faces, &mut out.faces);
            facet_menu(ui, 1, &why(tr!("Jellyfin 上的影片都還沒有標籤", "No videos on Jellyfin have tags yet")), d.tags.len() + 1, tags, &d.facet, s1, &mut change, &d.faces, &mut out.faces);
            facet_menu(ui, 2, &why(tr!("Jellyfin 上的影片都還沒有設年份", "No videos on Jellyfin have a year yet")), d.years.len(), years, &d.facet, s2, &mut change, &d.faces, &mut out.faces);
            d.facet_search = search;
            apply_facet(d, change, &mut out);
            ui.add_space(10.0);
            let tip = tr!(
                "找出 Jellyfin 上內容一樣的影片（同一支傳了兩次、重新壓縮過的），選要刪哪一份",
                "Find videos on Jellyfin with the same content (uploaded twice, re-encoded) and choose which copy to delete"
            );
            if super::toolbar::button(tr!("找重複的影片", "Find duplicates"), Icon::Copy, compact, tip).show(ui).clicked() {
                out.find_dupes = true;
            }
        }
    });
    if let Some(t) = d.search_at {
        if t.elapsed() >= SEARCH_WAIT {
            d.search_at = None;
            if d.search.trim() != d.searched {
                d.facet.clear();
                d.page = 0;
                out.reload = true;
            }
        } else {
            ui.ctx().request_repaint_after(SEARCH_WAIT);
        }
    }
    ui.add_space(4.0);
    // 目前位置：媒體庫 › family › 2024（點前面的回到那一層）；右邊是全部播放、符合幾支。
    // 依人物、標籤、年份找時，條件直接放在「媒體庫 ›」後面（不另外多一列，縮圖不會往下跳）
    let mut change = FacetChange::None;
    ui.horizontal(|ui| {
        ui.set_min_height(28.0);
        ui.spacing_mut().item_spacing = vec2(2.0, 4.0);
        if Btn::new(tr!("媒體庫", "Libraries")).icon(Icon::Folder).small().ghost().selected(top).show(ui).clicked() && !top {
            d.path.clear();
            d.facet.clear();
            d.search.clear();
            d.search_at = None;
            d.page = 0;
            out.reload = true;
        }
        if !d.searched.is_empty() {
            ui.label(theme::muted(ui, "›"));
            Btn::new(&trf!("搜尋「{}」", "Search “{}”", d.searched)).small().ghost().selected(true).show(ui);
        } else {
            // 媒體庫 › X Pron › 2024（點前面的回到那一層；有依條件找時條件留著，換成找那一層）
            let n = d.path.len();
            let facets = !d.facet.is_empty();
            for i in 0..n {
                ui.label(theme::muted(ui, "›"));
                let last = i + 1 == n;
                if Btn::new(&d.path[i].1).small().ghost().selected(last && !facets).show(ui).clicked() && !(last && !facets) {
                    d.path.truncate(i + 1);
                    if last {
                        // 依條件找時按目前這一層：移除條件，回到資料夾
                        d.facet.clear();
                    }
                    d.page = 0;
                    out.reload = true;
                }
            }
            // 依人物、標籤、年份找的：媒體庫 › X Pron › 小美 × 或 小明 × 且 2024 × 全部清除
            if facets {
                ui.label(theme::muted(ui, "›"));
                ui.add_space(4.0);
                let room = ui.available_width() - d.crumb_right_w - 16.0;
                facet_chips(ui, &d.facet, room, &mut change);
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            // 在合集、播放清單裡：「全部播放」照順序連續播放
            if in_list.is_some()
                && d.searched.is_empty()
                && d.facet.is_empty()
                && d.items.as_ref().is_some_and(|i| i.iter().any(|x| x.kind == JfKind::Video))
                && Btn::new(tr!("全部播放", "Play all")).icon(Icon::Play).small().tooltip(tr!("從第一支照順序連續播放", "Play everything in order from the first one")).show(ui).clicked()
            {
                out.play_all = true;
            }
            // 依條件找：寫符合幾支（滑鼠移上去看完整的條件）
            if !d.facet.is_empty() && d.searched.is_empty() {
                let mut what = facet_summary(&d.facet);
                if let Some((_, name)) = d.path.last().filter(|_| facet_scope(&d.path).is_some()) {
                    what = trf!("「{name}」裡{what}", "{what} in “{name}”");
                }
                if d.filter != JfFilter::All {
                    what = format!("{what}{}{}", tr!("・", " · "), d.filter.label());
                }
                let (text, tip) = match &d.items {
                    Some(_) => (trf!("符合 {} 支", "{} matching", d.total), trf!("符合 {} 支：{what}", "{} matching: {what}", d.total)),
                    None => (tr!("正在找…", "Finding…").to_string(), trf!("正在找{what}…", "Finding {what}…")),
                };
                ui.label(RichText::new(text).font(theme::font(12.5)).color(p.muted)).on_hover_text(tip);
            }
            d.crumb_right_w = ui.min_rect().width();
        });
    });
    apply_facet(d, change, &mut out);
    ui.add_space(6.0);
    let foot_h = 40.0;
    let table = Rect::from_min_max(ui.cursor().min, pos2(ui.max_rect().max.x, ui.max_rect().max.y - foot_h - 8.0));
    grid_zoom(ui, d, table);
    {
        let bui = &mut ui.new_child(UiBuilder::new().max_rect(table).layout(Layout::top_down(Align::Min)));
        match (&d.items, &d.error) {
            (_, Some(e)) => {
                bui.label(RichText::new(trf!("讀不到 Jellyfin：{e}", "Couldn't reach Jellyfin: {e}")).color(p.warn));
                if Btn::new(tr!("再試一次", "Try again")).small().show(bui).clicked() {
                    out.reload = true;
                }
            }
            (None, None) => {
                bui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tr!("正在讀取…", "Loading…"));
                });
            }
            (Some(items), None) if items.is_empty() => {
                let msg: String = if !d.facet.is_empty() && d.filter != JfFilter::All {
                    trf!(
                        "找不到符合這些條件、而且是「{}」的影片，可以移除幾個條件或把篩選改回「全部」試試",
                        "No videos match all of these and “{}”. Try removing a few, or set the filter back to All",
                        d.filter.label()
                    )
                } else if !d.facet.is_empty() {
                    tr!("找不到符合這些條件的影片，可以移除幾個條件試試", "No videos match all of these. Try removing a few").into()
                } else if !d.searched.is_empty() && d.filter != JfFilter::All {
                    tr!("找不到符合的影片（篩選不是「全部」）", "No matching videos (the filter isn't All)").into()
                } else if !d.searched.is_empty() {
                    tr!("找不到符合的影片", "No matching videos").into()
                } else if d.filter == JfFilter::All {
                    tr!("這裡沒有影片", "No videos here").into()
                } else {
                    tr!("這裡沒有符合的影片", "No matching videos here").into()
                };
                bui.label(RichText::new(msg).color(p.muted));
            }
            (Some(items), None) => {
                egui::ScrollArea::vertical().id_salt("jf-body").max_height(table.height()).auto_shrink([false, false]).show(bui, |ui| {
                    if d.at_top() {
                        // 最上層：繼續觀看、最近加入各一列，接著是媒體庫
                        for (title, list, salt) in [(tr!("繼續觀看", "Continue watching"), &d.resume, "resume"), (tr!("最近加入", "Recently added"), &d.latest, "latest")] {
                            if !list.is_empty() {
                                section(ui, title);
                                ui.push_id(salt, |ui| cells(ui, d, list, Some(1), &mut out.picked));
                            }
                        }
                        if !d.resume.is_empty() || !d.latest.is_empty() {
                            section(ui, tr!("媒體庫", "Libraries"));
                        }
                    }
                    if d.list_view && !d.at_top() {
                        ui.push_id("items", |ui| rows(ui, d, items, &mut out.picked));
                    } else {
                        ui.push_id("items", |ui| cells(ui, d, items, None, &mut out.picked));
                    }
                    // 還有沒讀的：捲到離底部不到一個畫面高時讀下一批，最下面顯示正在讀
                    if !d.at_top() && items.len() < d.total {
                        if d.loading_more {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(theme::muted(ui, tr!("正在讀取更多…", "Loading more…")));
                            });
                        }
                        if ui.cursor().min.y < ui.clip_rect().max.y + ui.clip_rect().height() {
                            out.more = true;
                        }
                    }
                });
            }
        }
    }
    // 下面：右邊換頁，左邊勾選的操作（和錄影、截圖分頁一樣）
    let foot = Rect::from_min_max(pos2(table.min.x, ui.max_rect().max.y - foot_h), ui.max_rect().max);
    ui.painter().hline(foot.x_range(), foot.min.y - 4.0, egui::Stroke::new(1.0, p.border));
    super::dialogs::toast_spot(ui.ctx(), pos2(foot.max.x, foot.min.y - 4.0));
    // 右下角：共幾個（不換頁，捲動時自動讀下一批）
    let pager_w = if !d.at_top() && d.items.is_some() {
        let right = &mut ui.new_child(UiBuilder::new().max_rect(foot).layout(Layout::right_to_left(Align::Center)));
        right.label(theme::muted(right, trf!("共 {} 個", "{} in all", d.total)));
        right.min_rect().width() + 16.0
    } else {
        0.0
    };
    let fui = &mut ui.new_child(UiBuilder::new().max_rect(Rect::from_min_max(foot.min, pos2(foot.max.x - pager_w, foot.max.y))).layout(Layout::left_to_right(Align::Center)));
    if let Some((done, total)) = busy {
        fui.spinner();
        let f = if total > 0 { done as f32 / total as f32 } else { 0.0 };
        fui.label(theme::muted(fui, trf!("正在整理 {done}/{total} 支（{:.0}%）…", "Organizing {done}/{total} ({:.0}%)…", f * 100.0)));
        let (bar, _) = fui.allocate_exact_size(vec2(90.0, 5.0), Sense::hover());
        fui.painter().rect_filled(bar, 2.5, p.border);
        fui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * f, bar.height())), 2.5, p.accent);
        if Btn::new(tr!("取消", "Cancel"))
            .small()
            .ghost()
            .tooltip(tr!("改完正在改的那一支就停；已經改的照樣保留", "Stops after the current video; videos already changed stay changed"))
            .show(fui)
            .clicked()
        {
            out.cancel_batch = true;
        }
        fui.add_space(10.0);
    }
    if let Some((i, n, name, pct)) = &subs {
        fui.spinner();
        let short: String = if name.chars().count() > 18 { name.chars().take(17).chain(['…']).collect() } else { name.clone() };
        let text = if *n > 1 {
            trf!("正在產生字幕：{short}（{i}/{n}）{pct}%", "Making subtitles: {short} ({i}/{n}) {pct}%")
        } else {
            trf!("正在產生字幕：{short} {pct}%", "Making subtitles: {short} {pct}%")
        };
        fui.label(theme::muted(fui, text)).on_hover_text(name);
        if Btn::new(tr!("取消", "Cancel")).small().ghost().show(fui).clicked() {
            out.cancel_subs = true;
        }
        fui.add_space(10.0);
    }
    if d.selected.is_empty() {
        fui.label(theme::muted(fui, tr!("多選小技巧", "Multi-select tips"))).on_hover_text(tr!(
            "滑鼠移到影片上按左上角的方塊，或按住 Ctrl 點影片，勾選好幾支一次下載或刪除；按住 Shift 按一下，從上次勾的那支到這一支全部勾起來",
            "Hover a video and click the box in its top-left corner, or Ctrl+click videos, to download or delete several at once. Shift+click selects everything from the last one you checked"
        ));
    } else {
        fui.label(RichText::new(trf!("已勾選 {} 支", "{} selected", d.selected.len())).font(theme::font_bold(13.0)));
        if Btn::new(tr!("整理人物、標籤…", "Organize people & tags…"))
            .icon(Icon::Edit)
            .small()
            .primary()
            .enabled(busy.is_none())
            .tooltip(if busy.is_some() {
                tr!("上一次的整理還沒改完", "The previous batch is still running")
            } else {
                tr!("一次加、移除勾選影片的人物和標籤，改年份", "Add or remove people and tags, or set the year, on all selected videos")
            })
            .show(fui)
            .clicked()
        {
            out.bulk_organize = true;
        }
        if Btn::new(tr!("產生字幕", "Make subtitles"))
            .icon(Icon::Captions)
            .small()
            .enabled(subs.is_none())
            .tooltip(if subs.is_some() {
                tr!("正在幫其他影片產生字幕", "Already making subtitles for other videos")
            } else {
                tr!("用這台電腦的語音辨識做字幕，傳回 Jellyfin（電視、手機上看也有字幕）", "Recognize speech on this computer and add the subtitles to Jellyfin (shown on TVs and phones too)")
            })
            .show(fui)
            .clicked()
        {
            out.picked.subs = Some(d.selected.values().cloned().collect());
        }
        let add = Btn::new(tr!("加進…", "Add to…")).small().menu().tooltip(tr!("加進合集或播放清單", "Add to a collection or playlist")).show(fui);
        // 勾了的影片只在用到時才複製（Ctrl+A 全選時每一格都複製整份很浪費）
        let chosen = || d.selected.values().cloned().collect::<Vec<JfItem>>();
        egui::Popup::menu(&add).show(|ui| add_menu(ui, d, &chosen(), &mut out.picked.add_to));
        if let Some((kind, _)) = d.path.last().and_then(|(id, _)| list_at(id)) {
            let here = if kind == JfList::Collection { tr!("從這個合集移除", "Remove from this collection") } else { tr!("從這個播放清單移除", "Remove from this playlist") };
            if Btn::new(here).small().tooltip(tr!("影片本身不會刪掉", "The videos themselves aren't deleted")).show(fui).clicked() {
                out.picked.remove_here = Some(chosen());
            }
        }
        if Btn::new(tr!("下載到這台電腦", "Download to this computer")).icon(Icon::Download).small().show(fui).clicked() {
            out.bulk_download = true;
        }
        if Btn::new(tr!("從 Jellyfin 刪除", "Delete from Jellyfin")).icon(Icon::Trash).small().danger().show(fui).clicked() {
            out.bulk_delete = true;
        }
        if Btn::new(tr!("取消勾選", "Clear selection")).small().ghost().show(fui).clicked() {
            d.selected.clear();
        }
    }
    out
}

/// 處理在分頁上按的（d 是從 app.jf_browse 拿出來畫的那一份，處理完放回去）
pub fn apply(app: &mut UiApp, mut d: JfBrowse, mut out: Panel) {
    if let Some((it, rect)) = out.picked.hover.take() {
        hover(app, &it, rect);
    }
    // 縮圖大小、清單檢視、排序、篩選、停在哪裡記到下次（有改才存）
    let sort_at = JfSort::ALL.iter().position(|s| *s == d.sort).unwrap_or(0) as u8;
    let filter_at = JfFilter::ALL.iter().position(|f| *f == d.filter).unwrap_or(0) as u8;
    if d.grid != app.s.jf_grid_size || d.list_view != app.s.jf_list_view || sort_at != app.s.jf_sort || filter_at != app.s.jf_filter || d.path != app.s.jf_path {
        app.s.jf_path = d.path.clone();
        app.s.jf_grid_size = d.grid;
        app.s.jf_list_view = d.list_view;
        app.s.jf_sort = sort_at;
        app.s.jf_filter = filter_at;
        app.save_settings();
    }
    if let Some(c) = out.picked.cursor.take() {
        d.cursor = Some(c);
    }
    if let Some(n) = out.picked.cols.take() {
        d.cols = n;
    }
    if std::mem::take(&mut out.picked.scrolled) {
        d.scroll_to_cursor = false;
    }
    let Panel { mut reload, more, mut picked, bulk_download, bulk_delete, bulk_organize, play_all, cancel_subs, cancel_batch, faces, find_dupes } = out;
    if let Some(s) = picked.sort.take() {
        d.sort = s;
        d.page = 0;
        reload = true;
    }

    if cancel_subs {
        if let Some(r) = &app.jf_subs {
            r.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
    if cancel_batch {
        if let Some(r) = &app.jf_batch_run {
            r.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
    if let Some(items) = picked.subs.take() {
        app.jf_browse = Some(d);
        return super::jellyfin_subs::start(app, items);
    }
    if play_all {
        let row: Vec<JfItem> = d.items.clone().unwrap_or_default();
        if let Some(first) = row.iter().find(|i| i.kind == JfKind::Video && i.duration > 0.0).cloned() {
            play(app, &d, &first, &row);
        }
        app.jf_browse = Some(d);
        return;
    }
    if let Some(a) = picked.add_to.take() {
        app.jf_browse = Some(d);
        return add_to_list(app, a);
    }
    if let Some(items) = picked.remove_here.take() {
        app.jf_browse = Some(d);
        // 一次移除很多支：先問（移除之後也可以按「復原」放回去）
        if items.len() >= 5 {
            let n = items.len();
            app.ask = Some(super::dialogs::Ask::confirm(
                trf!("從這裡移除 {n} 支影片？", "Remove {n} videos from here?"),
                tr!("影片本身不會刪掉，只是不在這個合集或播放清單裡。", "The videos themselves aren't deleted; they're just taken out of this collection or playlist."),
                tr!("移除", "Remove"),
                move |app, _| remove_here(app, items),
            ));
            return;
        }
        return remove_here(app, items);
    }
    if let Some(it) = picked.toggle {
        d.anchor = Some(it.id.clone());
        if d.selected.remove(&it.id).is_none() {
            d.selected.insert(it.id.clone(), it);
        }
    }
    // Shift＋按一下：同一排裡從上次勾的到這一支都勾起來（上次勾的不在這一排時只勾這一支）
    if let Some((it, row)) = picked.range {
        let from = d.anchor.as_ref().and_then(|a| row.iter().position(|r| &r.id == a));
        match (from, row.iter().position(|r| r.id == it.id)) {
            (Some(a), Some(b)) => {
                for r in row[a.min(b)..=a.max(b)].iter().filter(|r| r.kind == JfKind::Video) {
                    d.selected.insert(r.id.clone(), r.clone());
                }
            }
            _ => {
                d.anchor = Some(it.id.clone());
                d.selected.insert(it.id.clone(), it);
            }
        }
    }
    let selected: Vec<(String, String)> = d.selected.values().map(|i| (i.id.clone(), i.file_name.clone())).collect();
    if bulk_download {
        d.selected.clear();
        app.jf_browse = Some(d);
        return download(app, selected);
    }
    if bulk_organize {
        let (user, items) = (d.user.clone().unwrap_or_default(), d.selected.values().cloned().collect());
        let people = d.persons.iter().map(|(_, n, c)| (n.clone(), *c)).collect();
        let tags = d.tags.clone();
        app.jf_browse = Some(d);
        return super::jellyfin_batch::open(app, user, items, people, tags);
    }
    if bulk_delete {
        let names = d.selected.values().map(|i| (i.id.clone(), i.name.clone())).collect();
        app.jf_browse = Some(d);
        return confirm_delete_many(app, names);
    }
    if let Some(it) = picked.download {
        app.jf_browse = Some(d);
        return download(app, vec![(it.id, it.file_name)]);
    }
    if let Some(it) = picked.delete {
        app.jf_browse = Some(d);
        return confirm_delete(app, it.id, it.name);
    }
    if find_dupes {
        let user = d.user.clone().unwrap_or_default();
        app.jf_browse = Some(d);
        return super::jellyfin_dupes::open(app, user);
    }
    if let Some(it) = picked.edit {
        let (user, people, tags) = (d.user.clone().unwrap_or_default(), d.persons.iter().map(|(_, n, _)| n.clone()).collect(), d.tags.iter().map(|(t, _)| t.clone()).collect());
        app.jf_browse = Some(d);
        return super::jellyfin_edit::open(app, it.id, user, it.name, people, tags);
    }
    if let Some(it) = picked.open {
        match it.kind {
            JfKind::Folder => {
                d.facet.clear();
                d.path.push((it.id.clone(), it.name.clone()));
                d.search.clear();
                d.search_at = None;
                d.page = 0;
                reload = true;
            }
            JfKind::Video => {
                play(app, &d, &it, &picked.row);
                app.jf_browse = Some(d);
                return;
            }
        }
    }
    if reload {
        // 換了位置、搜尋：從新的那一層開始
        if d.search.trim().is_empty() {
            d.searched.clear();
        }
        app.jf_browse = Some(d);
        load(app);
        return;
    }
    let more = more && !d.loading_more && !reload;
    app.jf_browse = Some(d);
    for id in picked.posters {
        load_poster(app, &id);
    }
    let mut faces = faces;
    faces.dedup();
    for id in faces {
        load_face(app, &id);
    }
    if more {
        load_more(app);
    }
}

/// 重讀目前這一層（例如做好字幕後，CC 標記跟著出現）
pub fn reload(app: &mut UiApp) {
    if let Some(d) = app.jf_browse.as_mut() {
        d.page = 0;
        load(app);
    }
}

/// 重讀合集、播放清單的清單（「加進…」選單、最上層兩格的數字）；正在看這個合集、播放清單的話也重讀
fn reload_lists(app: &mut UiApp, list: Option<String>) {
    let Some(jf) = app.jellyfin() else { return };
    let Some(user) = app.jf_browse.as_ref().and_then(|d| d.user.clone()) else { return };
    app.spawn(
        async move { tokio::task::spawn_blocking(move || (jf.lists(&user, JfList::Collection).unwrap_or_default(), jf.lists(&user, JfList::Playlist).unwrap_or_default())).await.unwrap_or_default() },
        move |app, (cols, pls): (Vec<JfItem>, Vec<JfItem>)| {
            let Some(d) = app.jf_browse.as_mut() else { return };
            if let Some(items) = d.items.as_mut().filter(|_| d.path.is_empty() && d.searched.is_empty() && d.facet.is_empty()) {
                for t in list_tiles(cols.len(), pls.len()) {
                    if let Some(x) = items.iter_mut().find(|i| i.id == t.id) {
                        x.children = t.children;
                    }
                }
            }
            d.collections = cols;
            d.playlists = pls;
            let here = d.path.last().map(|(id, _)| real_id(id).to_string());
            if list.is_some() && here == list {
                d.page = 0;
                load(app);
            }
        },
    );
}

/// 加進合集、播放清單（新建的先問名稱）
fn add_to_list(app: &mut UiApp, a: AddTo) {
    let ids: Vec<String> = a.items.iter().filter(|i| i.kind == JfKind::Video).map(|i| i.id.clone()).collect();
    if ids.is_empty() {
        return;
    }
    let what = if a.kind == JfList::Collection { tr!("合集", "collection") } else { tr!("播放清單", "playlist") };
    let Some((list, name)) = a.to else {
        let title = if a.kind == JfList::Collection { tr!("新合集", "New collection") } else { tr!("新播放清單", "New playlist") };
        let kind = a.kind;
        app.ask = Some(super::dialogs::Ask::input(
            title,
            trf!("{what}的名稱（{} 支影片放進去）", "Name of the {what} ({} videos go in)", ids.len()),
            "",
            tr!("建立", "Create"),
            move |app, name| {
                if name.trim().is_empty() {
                    return;
                }
                let Some(jf) = app.jellyfin() else { return };
                let Some(user) = app.jf_browse.as_ref().and_then(|d| d.user.clone()) else { return };
                let n = ids.len();
                app.spawn(async move { tokio::task::spawn_blocking(move || jf.create_list(&user, kind, &name, &ids).map(|_| name)).await.unwrap_or_else(|e| Err(e.to_string())) }, move |app, r| {
                    match r {
                        Ok(name) => app.toast(trf!("已建立{what}「{name}」，放進 {n} 支影片", "Created the {what} “{name}” with {n} videos"), false),
                        Err(e) => app.toast(trf!("建立{what}失敗：{e}", "Couldn't create the {what}: {e}"), true),
                    }
                    reload_lists(app, None);
                });
            },
        ));
        return;
    };
    let Some(jf) = app.jellyfin() else { return };
    let Some(user) = app.jf_browse.as_ref().and_then(|d| d.user.clone()) else { return };
    let (kind, n, list2) = (a.kind, ids.len(), list.clone());
    app.spawn(async move { tokio::task::spawn_blocking(move || jf.add_to_list(&user, kind, &list2, &ids)).await.unwrap_or_else(|e| Err(e.to_string())) }, move |app, r| {
        match r {
            Ok(()) => app.toast(trf!("已把 {n} 支影片加進{what}「{name}」", "Added {n} videos to the {what} “{name}”"), false),
            Err(e) => app.toast(trf!("加進{what}「{name}」失敗：{e}", "Couldn't add to the {what} “{name}”: {e}"), true),
        }
        reload_lists(app, Some(list));
    });
}

/// 從目前這個合集、播放清單移除（影片本身不刪）
fn remove_here(app: &mut UiApp, items: Vec<JfItem>) {
    let Some(jf) = app.jellyfin() else { return };
    let Some(d) = app.jf_browse.as_mut() else { return };
    let Some((kind, list)) = d.path.last().and_then(|(id, _)| list_at(id)).map(|(k, x)| (k, x.to_string())) else { return };
    let ids: std::collections::HashSet<String> = items.iter().map(|i| i.id.clone()).collect();
    d.selected.retain(|id, _| !ids.contains(id));
    let n = items.len();
    let list2 = list.clone();
    let (user, back) = (d.user.clone().unwrap_or_default(), items.iter().map(|i| i.id.clone()).collect::<Vec<_>>());
    app.spawn(async move { tokio::task::spawn_blocking(move || jf.remove_from_list(kind, &list2, &items)).await.unwrap_or_else(|e| Err(e.to_string())) }, move |app, r| {
        let what = if kind == JfList::Collection { tr!("合集", "collection") } else { tr!("播放清單", "playlist") };
        match r {
            Ok(()) => {
                let undo_list = list.clone();
                app.toast_with(
                    trf!("已從{what}移除 {n} 支影片（影片本身還在）", "Removed {n} videos from the {what} (the videos themselves are kept)"),
                    false,
                    vec![super::ToastAction::new(tr!("復原", "Undo"), move |app| put_back(app, kind, user, undo_list, back))],
                );
            }
            Err(e) => app.toast(trf!("從{what}移除失敗：{e}", "Couldn't remove from the {what}: {e}"), true),
        }
        reload_lists(app, Some(list));
    });
}

/// 「復原」剛才移除的：放回同一個合集、播放清單（播放清單裡會排到最後面）
fn put_back(app: &mut UiApp, kind: JfList, user: String, list: String, ids: Vec<String>) {
    let Some(jf) = app.jellyfin() else { return };
    let list2 = list.clone();
    app.spawn(async move { tokio::task::spawn_blocking(move || jf.add_to_list(&user, kind, &list2, &ids)).await.unwrap_or_else(|e| Err(e.to_string())) }, move |app, r| {
        match r {
            Ok(()) => app.toast(tr!("已放回去", "Put back"), false),
            Err(e) => app.toast(trf!("放不回去：{e}", "Couldn't put them back: {e}"), true),
        }
        reload_lists(app, Some(list));
    });
}

/// 讀下一批接在後面（捲到快底時）；換了位置、搜尋的話丟掉
fn load_more(app: &mut UiApp) {
    let Some(jf) = app.jellyfin() else { return };
    let Some(d) = app.jf_browse.as_mut() else { return };
    let (Some(user), Some(start)) = (d.user.clone(), d.items.as_ref().map(Vec::len)) else { return };
    if start >= d.total || d.more_failed {
        return;
    }
    d.loading_more = true;
    let (seq, parent, search, sort, filter, facet) = (d.seq, d.path.last().map(|(id, _)| id.clone()), d.searched.clone(), d.sort, d.filter, d.facet.clone());
    app.spawn(
        async move { tokio::task::spawn_blocking(move || fetch(&jf, &user, parent.as_deref(), &search, start, sort, filter, &facet)).await.unwrap_or_else(|e| Err(e.to_string())) },
        move |app, r| {
            let Some(d) = app.jf_browse.as_mut().filter(|d| d.seq == seq) else { return };
            d.loading_more = false;
            match r {
                Ok((more, total)) => {
                    if let Some(items) = d.items.as_mut().filter(|i| i.len() == start) {
                        // 讀的時候伺服器上多了影片：同一支不重複列
                        let have: std::collections::HashSet<String> = items.iter().map(|i| i.id.clone()).collect();
                        items.extend(more.into_iter().filter(|i| !have.contains(&i.id)));
                        d.total = total;
                    }
                }
                Err(e) => {
                    d.more_failed = true;
                    app.toast(trf!("讀不到更多 Jellyfin 影片：{e}", "Couldn't load more Jellyfin videos: {e}"), true);
                }
            }
        },
    );
}

/// 滑鼠在 Jellyfin 影片的縮圖上：停住約 1 秒就在縮圖裡靜音播放（從停住的時間點），不然浮出滑過預覽
fn hover(app: &mut UiApp, it: &JfItem, rect: Rect) {
    let Some(jf) = app.jellyfin() else { return };
    let ctx = app.ctx.clone();
    let url = jf.stream_url(&it.id);
    if !super::hover_play::hover(&ctx, &it.id, rect, it.duration, |_| super::thumbs::hover_player(app, &url, it.duration, it.fps, (it.width, it.height))) {
        super::thumbs::jf_scrub_preview(app, &ctx, rect, it);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downloads_get_a_free_name() {
        let dir = std::env::temp_dir().join(format!("jf-free-name-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(free_name(&dir, "a.mp4"), Some(dir.join("a.mp4")));
        std::fs::write(dir.join("a.mp4"), "x").unwrap();
        assert_eq!(free_name(&dir, "a.mp4"), Some(dir.join("a (2).mp4")));
        assert_eq!(free_name(&dir, ""), Some(dir.join("jellyfin.mp4")));
        // 伺服器給的檔名不能跑到資料夾外面、不能變成程式
        assert_eq!(safe_file_name("..\\..\\Startup\\run.bat"), "run.bat.mp4");
        assert_eq!(safe_file_name("../x/影片.MKV"), "影片.MKV");
        assert_eq!(safe_file_name("C:a:b.mp4"), "C_a_b.mp4");
        assert_eq!(safe_file_name(".."), "jellyfin.mp4");
        assert_eq!(safe_file_name("練琴"), "練琴.mp4");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
