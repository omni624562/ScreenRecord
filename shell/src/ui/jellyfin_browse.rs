//! 檔案庫的「Jellyfin」分頁：列出伺服器上的媒體庫、資料夾、影片（家人用手機傳上去的也看得到），點了就在檢視器裡串流播放。
//! 不在家時只要連得到 Jellyfin 也能看。最上層先列「繼續觀看」「最近加入」；資料夾裡可以排序、只看沒看過的等。
//! 可以多選一次下載到這台電腦（下載回來就能剪輯）或刪除。見 core::jellyfin 的 views、items、stream_url

use super::theme::{self, Btn, Icon};
use super::{viewer, ToastAction, UiApp};
use eframe::egui::{self, pos2, vec2, Align, Align2, Layout, Rect, RichText, Sense, UiBuilder};
use screenrecorder_core::jellyfin::{self, Jellyfin, JfFacet, JfFilter, JfItem, JfKind, JfSort};
use screenrecorder_core::types::{Chapter, LibraryEntry, MediaInfo};
use screenrecorder_core::{tr, trf};
use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

/// 一頁幾個
const PAGE: usize = 60;
/// 每格最小的寬度（縮圖 16:9）：縮圖小、中、大
const CELL_W: [f32; 3] = [150.0, 196.0, 300.0];
/// 「繼續觀看」「最近加入」各讀幾個
const ROW_ITEMS: usize = 12;
/// 搜尋框停止打字多久之後才找（每打一個字就問伺服器太多次）
const SEARCH_WAIT: Duration = Duration::from_millis(400);

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
    page: usize,
    sort: JfSort,
    filter: JfFilter,
    error: Option<String>,
    seq: u64,
    posters: HashMap<String, Poster>,
    /// 勾選的影片（編號 → 那支影片）
    selected: BTreeMap<String, JfItem>,
    /// 上次勾或取消勾的那一支（Shift＋點一下：從它選到點的那一支）
    anchor: Option<String>,
    /// 依人物、標籤、年份找（找整個伺服器的影片）：勾了的條件，同一類任一個符合、不同類都要符合；空的 = 沒有用
    facet: Vec<JfFacet>,
    /// 伺服器上的人物（編號, 名稱, 幾支影片）、標籤（名稱, 幾支）、年份（年, 幾支）（打開時讀一次，改了影片資訊後重讀）；
    /// 人物、標籤用得多的在前，年份新的在前；讀不到次數時是 0
    persons: Vec<(String, String, usize)>,
    tags: Vec<(String, usize)>,
    years: Vec<(u32, usize)>,
    /// 「人物」「標籤」「年份」選單裡的搜尋框
    facet_search: [String; 3],
    facets_loaded: bool,
    /// 讀不到標籤、年份時的原因（選單變灰時滑鼠移上去看得到）
    facets_error: Option<String>,
    /// 縮圖大小：0 小、1 中、2 大（記在設定裡）
    grid: u8,
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
    app.jf_browse = Some(JfBrowse { grid: app.s.jf_grid_size, ..Default::default() });
    load(app);
}

/// 讀到的一層：（使用者, 東西, 全部幾個, 繼續觀看, 最近加入）
type Loaded = (String, Vec<JfItem>, usize, Vec<JfItem>, Vec<JfItem>);

/// 讀目前這一層（或搜尋結果）的這一頁；最上層連「繼續觀看」「最近加入」一起讀
fn load(app: &mut UiApp) {
    let Some(jf) = app.jellyfin() else { return };
    let preferred = app.s.jellyfin_user.clone();
    let Some(d) = app.jf_browse.as_mut() else { return };
    d.seq += 1;
    d.items = None;
    d.error = None;
    d.searched = d.search.trim().to_string();
    let (seq, user, parent, search, start, sort, filter, facet) =
        (d.seq, d.user.clone(), d.path.last().map(|(id, _)| id.clone()), d.searched.clone(), d.page * PAGE, d.sort, d.filter, d.facet.clone());
    let want_facets = !d.facets_loaded;
    d.facets_loaded = true;
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || -> Result<Loaded, String> {
                let user = match user {
                    Some(u) => u,
                    None => jf.browse_user(&preferred)?,
                };
                if parent.is_none() && search.is_empty() && facet.is_empty() {
                    let views = jf.views(&user)?;
                    let n = views.len();
                    // 舊版 Jellyfin 沒有這兩個也沒關係：只是不列
                    let resume = jf.resume_items(&user, ROW_ITEMS).unwrap_or_default();
                    let latest = jf.latest_items(&user, ROW_ITEMS).unwrap_or_default();
                    return Ok((user, views, n, resume, latest));
                }
                let (items, total) = jf.items(&user, parent.as_deref(), &search, start, PAGE, sort, filter, &facet)?;
                Ok((user, items, total, vec![], vec![]))
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        },
        move |app, r| {
            let Some(d) = app.jf_browse.as_mut().filter(|d| d.seq == seq) else { return };
            match r {
                Ok((user, items, total, resume, latest)) => {
                    d.user = Some(user);
                    d.items = Some(items);
                    d.total = total;
                    d.resume = resume;
                    d.latest = latest;
                }
                Err(e) => d.error = Some(e),
            }
        },
    );
    if want_facets {
        load_facets(app);
    }
}

/// 讀到的人物（編號, 名稱, 幾支）與（標籤、年份）或讀不到的原因
type Facets = (Vec<(String, String, usize)>, Result<(Vec<(String, usize)>, Vec<(u32, usize)>), String>);

/// 讀伺服器上的人物、標籤、年份和各用在幾支影片（「依這些找」的選單）；舊版 Jellyfin 讀不到就不列。
/// 次數讀不到時改問只有名稱的清單
fn load_facets(app: &mut UiApp) {
    let Some(jf) = app.jellyfin() else { return };
    let preferred = app.s.jellyfin_user.clone();
    let user = app.jf_browse.as_ref().and_then(|d| d.user.clone());
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || {
                let user = match user {
                    Some(u) => u,
                    None => jf.browse_user(&preferred).unwrap_or_default(),
                };
                let persons = jf.persons(&user).unwrap_or_default();
                let (persons, rest) = match jf.usage(&user) {
                    Ok(u) => {
                        let count: HashMap<String, usize> = u.people.iter().map(|(n, c)| (jellyfin::name_key(n), *c)).collect();
                        let mut persons: Vec<_> = persons.into_iter().map(|(id, n)| (id, count.get(&jellyfin::name_key(&n)).copied().unwrap_or(0), n)).collect();
                        // 用得多的在前（persons 已經依名稱排，sort_by 是穩定的）
                        persons.sort_by_key(|a| std::cmp::Reverse(a.1));
                        (persons.into_iter().map(|(id, c, n)| (id, n, c)).collect(), Ok((u.tags, u.years)))
                    }
                    Err(e) => {
                        screenrecorder_core::info!("[Jellyfin] 讀不到人物、標籤、年份的使用次數：{e}");
                        let rest = jf.facets(&user).map(|(t, y)| (t.into_iter().map(|t| (t, 0)).collect(), y.into_iter().map(|y| (y, 0)).collect()));
                        (persons.into_iter().map(|(id, n)| (id, n, 0)).collect(), rest)
                    }
                };
                (persons, rest)
            })
            .await
            .unwrap_or_else(|e| (vec![], Err(e.to_string())))
        },
        |app, (persons, facets): Facets| {
            if let Some(d) = app.jf_browse.as_mut() {
                d.persons = persons;
                match facets {
                    Ok((tags, years)) => {
                        d.tags = tags;
                        d.years = years;
                        d.facets_error = None;
                    }
                    Err(e) => d.facets_error = Some(e),
                }
            }
        },
    );
}

/// 人物、標籤改名之後重讀「依這些找」的清單（Jellyfin 分頁沒開時不用）
pub fn reload_facets(app: &mut UiApp) {
    if app.jf_browse.is_some() {
        load_facets(app);
    }
}

/// 在「影片資訊」改好了：名稱跟著換、縮圖重讀（可能換了封面），人物、標籤重讀；正在播這支的話標題也換
pub fn edited(app: &mut UiApp, id: &str, title: &str) {
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
    load_facets(app);
}

/// 伺服器上已經有的人物、標籤（影片資訊對話框點一下加入）；Jellyfin 分頁沒開時空的
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
                let bytes = jf.poster(&key)?;
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
            }
        },
    );
}

/// 從 Jellyfin 刪掉這支影片（先確認）：伺服器上的檔案一起刪，不能復原；這台電腦上的檔案不動
pub fn confirm_delete(app: &mut UiApp, id: String, name: String) {
    confirm_delete_many(app, vec![(id, name)]);
}

/// 一次刪掉好幾支（找重複的視窗用）
pub fn confirm_delete_list(app: &mut UiApp, list: Vec<(String, String)>) {
    confirm_delete_many(app, list);
}

/// 一次刪掉好幾支（先確認一次）。刪好之後瀏覽清單拿掉它們，檢視器正在播其中一支就關掉
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
                    let cancelled = matches!(&r, Err(e) if e.contains(tr!("已取消", "Cancelled")));
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
    app.toast(trf!("正在下載「{file}」，下載好就打開", "Downloading “{file}”; it opens when done"), false);
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || -> Result<std::path::PathBuf, String> {
                match free_name(&dir, &file) {
                    Some(to) => jf.download(&id, &to).map(|_| to),
                    None => Ok(dir.join(&file)),
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
            Err(e) => app.toast(trf!("沒有下載完：{e}", "The download didn't finish: {e}"), true),
        },
    );
}

/// 下載的檔名：還沒有就用原本的；同名同大小（已經下載過）None；同名不同的加上 (2)、(3)…
fn free_name(dir: &std::path::Path, file: &str) -> Option<std::path::PathBuf> {
    let file = if file.trim().is_empty() { "jellyfin.mp4" } else { file };
    let first = dir.join(file);
    if !first.exists() {
        return Some(first);
    }
    let (stem, ext) = file.rsplit_once('.').map_or((file, String::new()), |(s, e)| (s, format!(".{e}")));
    (2..1000).map(|i| dir.join(format!("{stem} ({i}){ext}"))).find(|p| !p.exists())
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
    /// Shift＋點一下：（點的那一支, 它所在的那一排）
    range: Option<(JfItem, Vec<JfItem>)>,
    posters: Vec<String>,
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
    for row in items.chunks(cols).take(rows.unwrap_or(usize::MAX)) {
        let (row_rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), cell_h), Sense::hover());
        for (i, it) in row.iter().enumerate() {
            let cell = Rect::from_min_size(pos2(row_rect.min.x + i as f32 * (cell_w + gap), row_rect.min.y), vec2(cell_w, cell_h));
            let resp = ui.interact(cell, ui.id().with(("jf-cell", &it.id)), Sense::click());
            let img = Rect::from_min_size(cell.min, vec2(cell_w, img_h));
            let video = it.kind == JfKind::Video;
            let on = d.selected.contains_key(&it.id);
            let painter = ui.painter();
            painter.rect_filled(img, 8.0, p.surface2);
            match d.posters.get(&it.id) {
                Some(Poster::Ready(tex)) => theme::paint_thumb_cover(painter, img, tex),
                None if it.has_image && clip.intersects(cell) => out.posters.push(it.id.clone()),
                _ => {}
            }
            if !matches!(d.posters.get(&it.id), Some(Poster::Ready(_))) {
                let icon = if video { Icon::Video } else { Icon::Folder };
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
            if video && (hovering || on || picking) {
                let r = ui.interact(check, ui.id().with(("jf-check", &it.id)), Sense::click()).on_hover_text(tr!("勾選（一次下載、刪除好幾支）", "Select (to download or delete several at once)"));
                let painter = ui.painter();
                painter.rect_filled(check, 5.0, if on { p.accent } else { egui::Color32::from_black_alpha(150) });
                painter.rect_stroke(check, 5.0, egui::Stroke::new(1.5, egui::Color32::WHITE), egui::StrokeKind::Inside);
                if on {
                    theme::paint_icon(painter, check.shrink(4.0), Icon::Check, egui::Color32::WHITE);
                }
                if r.clicked() {
                    out.toggle = Some(it.clone());
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
            if hovering && video && !picking {
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
                let g = painter.layout_no_wrap(t.to_string(), theme::font_bold(11.0), p.ok);
                let r = Rect::from_min_size(pos2(img.max.x - g.size().x - 14.0, img.min.y + 6.0), g.size() + vec2(8.0, 4.0));
                painter.rect_filled(r, 6.0, p.ok_soft);
                painter.galley(r.min + vec2(4.0, 2.0), g, p.ok);
            }
            let name = painter.layout(it.name.clone(), theme::font_bold(12.5), p.text, cell_w);
            let name_rect = Rect::from_min_size(pos2(cell.min.x, img.max.y + 4.0), vec2(cell_w, 18.0));
            painter.with_clip_rect(name_rect).galley(name_rect.min, name, p.text);
            let sub = match it.kind {
                JfKind::Folder if it.children > 0 => trf!("{} 個", "{} items", it.children),
                JfKind::Folder => String::new(),
                JfKind::Video => [it.date.clone(), if it.duration > 0.0 { clock(it.duration) } else { String::new() }].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · "),
            };
            painter.text(pos2(cell.min.x, img.max.y + 24.0), Align2::LEFT_TOP, sub, theme::font(11.5), p.muted);
            let resp = resp.on_hover_text(&it.name);
            if resp.clicked() && !used {
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
        JfFacet::Person(..) => 0,
        JfFacet::Tag(_) => 1,
        JfFacet::Year(_) => 2,
    }
}

fn facet_name(f: &JfFacet) -> String {
    match f {
        JfFacet::Person(_, n) => n.clone(),
        JfFacet::Tag(t) => t.clone(),
        JfFacet::Year(y) => y.to_string(),
    }
}

/// 套用「依這些找」的改變：找整個伺服器（離開資料夾、搜尋）；全部拿掉時回到最上層
fn apply_facet(d: &mut JfBrowse, change: FacetChange, out: &mut Panel) {
    match change {
        FacetChange::None => return,
        FacetChange::Toggle(f) => match d.facet.iter().position(|x| *x == f) {
            Some(i) => {
                d.facet.remove(i);
            }
            None => d.facet.push(f),
        },
        FacetChange::ClearKind(k) => d.facet.retain(|f| kind_of(f) != k),
        FacetChange::ClearAll => d.facet.clear(),
    }
    d.path.clear();
    d.search.clear();
    d.search_at = None;
    d.filter = JfFilter::All;
    d.page = 0;
    out.reload = true;
}

/// 「人物 ⌄」「標籤 ⌄」「年份 ⌄」其中一個：按鈕上的藍色數字是勾了幾個；打開是搜尋框和勾選清單（後面寫用在幾支影片），
/// 勾了馬上套用、清單不關（可以連續勾），最下面可以清除這一類。rows：（名稱, 幾支, 條件）
fn facet_menu(ui: &mut egui::Ui, kind: u8, empty: &str, rows: Vec<(String, usize, JfFacet)>, chosen: &[JfFacet], search: &mut String, change: &mut FacetChange) {
    let p = theme::pal(ui);
    let label = [tr!("人物", "People"), tr!("標籤", "Tags"), tr!("年份", "Year")][kind as usize];
    let n = chosen.iter().filter(|f| kind_of(f) == kind).count();
    let b = Btn::new(label).menu().small().selected(n > 0).badge(n).enabled(!rows.is_empty()).tooltip(if rows.is_empty() { empty } else { "" }).show(ui);
    egui::Popup::menu(&b).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        // 寬度固定：清單在選單裡，靠右的東西不能讓它每一格越撐越寬
        const W: f32 = 300.0;
        ui.set_width(W);
        ui.set_max_width(W);
        let hint = [tr!("搜尋人物…", "Search people…"), tr!("搜尋標籤…", "Search tags…"), tr!("搜尋年份…", "Search years…")][kind as usize];
        ui.add(egui::TextEdit::singleline(search).hint_text(hint).desired_width(W));
        ui.add_space(4.0);
        let q = jellyfin::name_key(search);
        egui::ScrollArea::vertical().id_salt(("jf-facet", kind)).max_height(300.0).show(ui, |ui| {
            let mut any = false;
            for (name, c, f) in rows.iter().filter(|(n, ..)| q.is_empty() || jellyfin::name_key(n).contains(&q)) {
                any = true;
                // 每一列固定大小：勾選框＋名稱、右邊的支數（畫上去，選單才不會越撐越寬）
                let (row, _) = ui.allocate_exact_size(vec2(W - 16.0, 26.0), Sense::hover());
                let cw = if *c > 0 { 56.0 } else { 0.0 };
                let mut on = chosen.contains(f);
                let shown: String = if name.chars().count() > 24 { name.chars().take(23).chain(['…']).collect() } else { name.clone() };
                let cb = Rect::from_min_max(row.min, pos2(row.max.x - cw, row.max.y));
                let r = ui.scope_builder(UiBuilder::new().max_rect(cb).layout(Layout::left_to_right(Align::Center)), |ui| theme::check(ui, &mut on, shown)).inner;
                if r.changed() {
                    *change = FacetChange::Toggle(f.clone());
                }
                if *c > 0 {
                    ui.painter().text(pos2(row.max.x - 4.0, row.center().y), Align2::RIGHT_CENTER, trf!("{c} 支", "{c}"), theme::font(12.0), p.muted);
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

/// 條件列：每個條件一顆（按一下拿掉），同一類之間寫「或」、不同類之間寫「且」，最後是「全部清除」
fn facet_chips(ui: &mut egui::Ui, chosen: &[JfFacet], change: &mut FacetChange) {
    let p = theme::pal(ui);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        let mut first = true;
        for kind in 0..3u8 {
            let list: Vec<&JfFacet> = chosen.iter().filter(|f| kind_of(f) == kind).collect();
            if list.is_empty() {
                continue;
            }
            if !first {
                ui.label(RichText::new(tr!("且", "and")).font(theme::font_bold(12.5)).color(p.text));
            }
            first = false;
            for (i, f) in list.into_iter().enumerate() {
                if i > 0 {
                    ui.label(RichText::new(tr!("或", "or")).font(theme::font(12.0)).color(p.muted));
                }
                let kind_name = [tr!("人物", "Person"), tr!("標籤", "Tag"), tr!("年份", "Year")][kind as usize];
                let tip = trf!("{kind_name}：{}，按一下拿掉這個條件", "{kind_name}: {}. Click to remove this condition", facet_name(f));
                if remove_chip(ui, &facet_name(f)).on_hover_text(tip).clicked() {
                    *change = FacetChange::Toggle(f.clone());
                }
            }
        }
        ui.add_space(6.0);
        if Btn::new(tr!("全部清除", "Clear all")).small().ghost().tooltip(tr!("拿掉所有條件，回到媒體庫", "Remove all conditions and go back to the libraries")).show(ui).clicked() {
            *change = FacetChange::ClearAll;
        }
    });
}

/// 藍色小標籤，後面有 ×：整顆按一下拿掉
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
    let names = |k: u8| chosen.iter().filter(|f| kind_of(f) == k).map(facet_name).collect::<Vec<_>>();
    let or = tr!("或", " or ");
    let mut parts = vec![];
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
    picked: Picked,
    bulk_download: bool,
    bulk_delete: bool,
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
pub fn panel(ui: &mut egui::Ui, d: &mut JfBrowse) -> Panel {
    let mut out = Panel::default();
    let p = theme::pal(ui);
    // 搜尋整個伺服器（打字停一下就找，Enter 馬上找）、依人物 / 標籤 / 年份找、找重複的影片
    ui.horizontal(|ui| {
        let r = ui.add(
            egui::TextEdit::singleline(&mut d.search)
                .hint_text(tr!("搜尋 Jellyfin 上的影片", "Search videos on Jellyfin"))
                .desired_width((ui.available_width() - 470.0).clamp(160.0, 340.0))
                .min_size(vec2(0.0, 30.0))
                .vertical_align(Align::Center),
        );
        if r.changed() {
            d.search_at = Some(Instant::now());
        }
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            d.search_at = Some(Instant::now() - SEARCH_WAIT);
        }
        if !d.search.is_empty() && Btn::icon_only(Icon::Close).small().ghost().tooltip(tr!("清除搜尋", "Clear search")).show(ui).clicked() {
            d.search.clear();
            d.search_at = Some(Instant::now() - SEARCH_WAIT);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if Btn::new(tr!("找重複的影片", "Find duplicates"))
                .icon(Icon::Copy)
                .small()
                .ghost()
                .tooltip(tr!(
                    "找出 Jellyfin 上內容一樣的影片（同一支傳了兩次、重新壓縮過的），選要刪哪一份",
                    "Find videos on Jellyfin with the same content (uploaded twice, re-encoded) and choose which copy to delete"
                ))
                .show(ui)
                .clicked()
            {
                out.find_dupes = true;
            }
            ui.add_space(8.0);
            // 人物、標籤、年份各一個勾選清單（勾了馬上套用、清單不關）；按鈕上的藍色數字是勾了幾個
            let mut change = FacetChange::None;
            let why = |none: &str| match &d.facets_error {
                Some(e) => trf!("讀不到 Jellyfin 的標籤、年份：{e}", "Couldn't read tags and years from Jellyfin: {e}"),
                None => none.to_string(),
            };
            let years: Vec<_> = d.years.iter().map(|(y, c)| (y.to_string(), *c, JfFacet::Year(*y))).collect();
            let tags: Vec<_> = d.tags.iter().map(|(t, c)| (t.clone(), *c, JfFacet::Tag(t.clone()))).collect();
            let people: Vec<_> = d.persons.iter().map(|(id, n, c)| (n.clone(), *c, JfFacet::Person(id.clone(), n.clone()))).collect();
            let [s0, s1, s2] = &mut d.facet_search;
            facet_menu(ui, 2, &why(tr!("Jellyfin 上的影片都還沒有設年份", "No videos on Jellyfin have a year yet")), years, &d.facet, s2, &mut change);
            facet_menu(ui, 1, &why(tr!("Jellyfin 上的影片都還沒有標籤", "No videos on Jellyfin have tags yet")), tags, &d.facet, s1, &mut change);
            facet_menu(ui, 0, tr!("影片還沒有設人物", "No videos have people yet"), people, &d.facet, s0, &mut change);
            ui.label(theme::muted(ui, tr!("依這些找：", "Find by:")));
            apply_facet(d, change, &mut out);
        });
    });
    // 勾了條件時：一列列出所有條件（每個可以按 × 拿掉；同一類之間「或」、不同類之間「且」）
    if !d.facet.is_empty() {
        ui.add_space(2.0);
        let mut change = FacetChange::None;
        facet_chips(ui, &d.facet, &mut change);
        apply_facet(d, change, &mut out);
    }
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
    // 目前位置：媒體庫 › family › 2024（點前面的回到那一層）；右邊排序、篩選
    ui.horizontal(|ui| {
        ui.set_min_height(28.0);
        ui.spacing_mut().item_spacing = vec2(2.0, 4.0);
        let top = d.at_top();
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
        } else if !d.facet.is_empty() {
            // 依人物、標籤、年份找的：媒體庫 › 符合 12 支：有小美或小明、2024 年的影片
            ui.label(theme::muted(ui, "›"));
            let what = facet_summary(&d.facet);
            let text = match &d.items {
                Some(_) => trf!("符合 {} 支：{what}", "{} matching: {what}", d.total),
                None => trf!("正在找{what}…", "Finding {what}…"),
            };
            let short: String = if text.chars().count() > 70 { text.chars().take(69).chain(['…']).collect() } else { text.clone() };
            Btn::new(short).small().ghost().selected(true).tooltip(text).show(ui);
        } else {
            let n = d.path.len();
            for i in 0..n {
                ui.label(theme::muted(ui, "›"));
                let last = i + 1 == n;
                if Btn::new(&d.path[i].1).small().ghost().selected(last).show(ui).clicked() && !last {
                    d.path.truncate(i + 1);
                    d.page = 0;
                    out.reload = true;
                }
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            // 縮圖大小（也可以在縮圖上按住 Ctrl 轉滾輪）
            let tip = tr!(
                "縮圖大小：小的一次看得到比較多支。也可以在縮圖上按住 Ctrl 轉滾輪調整",
                "Thumbnail size: smaller shows more at once. You can also hold Ctrl and turn the mouse wheel over the thumbnails"
            );
            theme::segmented(ui, &mut d.grid, &[(2u8, tr!("大", "L")), (1, tr!("中", "M")), (0, tr!("小", "S"))], true);
            ui.label(theme::muted(ui, tr!("縮圖", "Thumbnails"))).on_hover_text(tip);
            if top {
                return;
            }
            ui.add_space(6.0);
            theme::field_style(ui);
            let (sort, filter) = (d.sort, d.filter);
            egui::ComboBox::from_id_salt("jf-sort").selected_text(d.sort.label()).width(130.0).show_ui(ui, |ui| {
                for s in JfSort::ALL {
                    ui.selectable_value(&mut d.sort, s, s.label());
                }
            });
            // 篩選只在資料夾裡（列出這個資料夾底下符合的影片）
            if d.searched.is_empty() && d.facet.is_empty() {
                egui::ComboBox::from_id_salt("jf-filter")
                    .selected_text(d.filter.label())
                    .width(120.0)
                    .show_ui(ui, |ui| {
                        for f in JfFilter::ALL {
                            ui.selectable_value(&mut d.filter, f, f.label());
                        }
                    })
                    .response
                    .on_hover_text(tr!("不是「全部」時，列出這個資料夾底下（含子資料夾）符合的影片", "Anything but “All” lists matching videos in this folder and its subfolders"));
            }
            if (sort, filter) != (d.sort, d.filter) {
                d.page = 0;
                out.reload = true;
            }
        });
    });
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
                let msg = if !d.facet.is_empty() {
                    tr!("找不到符合這些條件的影片，可以拿掉幾個條件試試", "No videos match all of these. Try removing a few")
                } else if !d.searched.is_empty() {
                    tr!("找不到符合的影片", "No matching videos")
                } else if d.filter == JfFilter::All {
                    tr!("這裡沒有影片", "No videos here")
                } else {
                    tr!("這裡沒有符合的影片", "No matching videos here")
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
                    ui.push_id("items", |ui| cells(ui, d, items, None, &mut out.picked));
                });
            }
        }
    }
    // 下面：右邊換頁，左邊勾選的操作（和錄影、截圖分頁一樣）
    let foot = Rect::from_min_max(pos2(table.min.x, ui.max_rect().max.y - foot_h), ui.max_rect().max);
    ui.painter().hline(foot.x_range(), foot.min.y - 4.0, egui::Stroke::new(1.0, p.border));
    let pages = d.total.div_ceil(PAGE).max(1);
    let pager_w = if !d.at_top() && d.items.is_some() {
        let right = &mut ui.new_child(UiBuilder::new().max_rect(foot).layout(Layout::right_to_left(Align::Center)));
        if pages > 1 {
            if Btn::icon_only(Icon::ChevR).small().ghost().enabled(d.page + 1 < pages).tooltip(tr!("下一頁", "Next page")).show(right).clicked() {
                d.page += 1;
                out.reload = true;
            }
            right.label(trf!("第 {} / {} 頁", "Page {} / {}", d.page + 1, pages));
            if Btn::icon_only(Icon::ChevL).small().ghost().enabled(d.page > 0).tooltip(tr!("上一頁", "Previous page")).show(right).clicked() {
                d.page -= 1;
                out.reload = true;
            }
        }
        right.label(theme::muted(right, trf!("共 {} 個", "{} in all", d.total)));
        right.min_rect().width() + 16.0
    } else {
        0.0
    };
    let fui = &mut ui.new_child(UiBuilder::new().max_rect(Rect::from_min_max(foot.min, pos2(foot.max.x - pager_w, foot.max.y))).layout(Layout::left_to_right(Align::Center)));
    if d.selected.is_empty() {
        fui.label(theme::muted(fui, tr!("多選小技巧", "Multi-select tips"))).on_hover_text(tr!(
            "滑鼠移到影片上按左上角的方塊，或按住 Ctrl 點影片，勾選好幾支一次下載或刪除；按住 Shift 點一下，從上次勾的那支到這一支全部勾起來",
            "Hover a video and click the box in its top-left corner, or Ctrl+click videos, to download or delete several at once. Shift+click selects everything from the last one you checked"
        ));
    } else {
        fui.label(RichText::new(trf!("已勾選 {} 支", "{} selected", d.selected.len())).font(theme::font_bold(13.0)));
        if Btn::new(tr!("下載到這台電腦", "Download to this computer")).icon(Icon::Download).small().primary().show(fui).clicked() {
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
pub fn apply(app: &mut UiApp, mut d: JfBrowse, out: Panel) {
    if d.grid != app.s.jf_grid_size {
        app.s.jf_grid_size = d.grid;
        app.save_settings();
    }
    let Panel { mut reload, picked, bulk_download, bulk_delete, find_dupes } = out;
    if let Some(it) = picked.toggle {
        d.anchor = Some(it.id.clone());
        if d.selected.remove(&it.id).is_none() {
            d.selected.insert(it.id.clone(), it);
        }
    }
    // Shift＋點一下：同一排裡從上次勾的到這一支都勾起來（上次勾的不在這一排時只勾這一支）
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
    app.jf_browse = Some(d);
    for id in picked.posters {
        load_poster(app, &id);
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
        let _ = std::fs::remove_dir_all(&dir);
    }
}
