//! 瀏覽 Jellyfin：列出伺服器上的媒體庫、資料夾、影片（家人用手機傳上去的也看得到），點了就在檢視器裡串流播放。
//! 不在家時只要連得到 Jellyfin 也能看。最上層先列「繼續觀看」「最近加入」；資料夾裡可以排序、只看沒看過的等。
//! 可以多選一次下載到這台電腦（下載回來就能剪輯）或刪除。見 core::jellyfin 的 views、items、stream_url

use super::theme::{self, Btn, Icon};
use super::{viewer, ToastAction, UiApp};
use eframe::egui::{self, pos2, vec2, Align, Align2, Id, Layout, Rect, RichText, Sense};
use screenrecorder_core::jellyfin::{Jellyfin, JfFacet, JfFilter, JfItem, JfKind, JfSort};
use screenrecorder_core::types::{Chapter, LibraryEntry, MediaInfo};
use screenrecorder_core::{tr, trf};
use std::collections::{BTreeMap, HashMap};

/// 一頁幾個
const PAGE: usize = 60;
/// 每格的寬度（縮圖 16:9）
const CELL_W: f32 = 196.0;
/// 「繼續觀看」「最近加入」各讀幾個
const ROW_ITEMS: usize = 12;

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
    /// 依人物、標籤、年份找（找整個伺服器的影片）
    facet: Option<JfFacet>,
    /// 伺服器上的人物（編號, 名稱）、標籤、年份（打開時讀一次，改了影片資訊後重讀）
    persons: Vec<(String, String)>,
    tags: Vec<String>,
    years: Vec<u32>,
    facets_loaded: bool,
}

impl JfBrowse {
    /// 最上層（媒體庫清單，沒有搜尋）
    fn at_top(&self) -> bool {
        self.path.is_empty() && self.searched.is_empty() && self.facet.is_none()
    }

    /// 這支影片在畫面上的每一份（同一支可能同時在「繼續觀看」和資料夾裡）
    fn each_mut(&mut self, id: &str, mut f: impl FnMut(&mut JfItem)) {
        for it in self.items.iter_mut().flatten().chain(self.resume.iter_mut()).chain(self.latest.iter_mut()).filter(|i| i.id == id) {
            f(it);
        }
    }
}

/// 打開瀏覽視窗（沒設定 Jellyfin 時提示到設定填）
pub fn open(app: &mut UiApp) {
    if app.jellyfin().is_none() {
        return app.toast(tr!("要先在「設定 → 存檔與分享」填 Jellyfin 的網址和 API 金鑰", "Fill in the Jellyfin address and API key in Settings → Saving & sharing first"), true);
    }
    app.jf_browse = Some(JfBrowse::default());
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
                if parent.is_none() && search.is_empty() && facet.is_none() {
                    let views = jf.views(&user)?;
                    let n = views.len();
                    // 舊版 Jellyfin 沒有這兩個也沒關係：只是不列
                    let resume = jf.resume_items(&user, ROW_ITEMS).unwrap_or_default();
                    let latest = jf.latest_items(&user, ROW_ITEMS).unwrap_or_default();
                    return Ok((user, views, n, resume, latest));
                }
                let (items, total) = jf.items(&user, parent.as_deref(), &search, start, PAGE, sort, filter, facet.as_ref())?;
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

/// 讀伺服器上的人物、標籤、年份（「依人物 / 標籤 / 年份找」的選單）；舊版 Jellyfin 讀不到就不列
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
                let (tags, years) = jf.facets(&user).unwrap_or_default();
                (persons, tags, years)
            })
            .await
            .unwrap_or_default()
        },
        |app, (persons, tags, years)| {
            if let Some(d) = app.jf_browse.as_mut() {
                d.persons = persons;
                d.tags = tags;
                d.years = years;
            }
        },
    );
}

/// 在「影片資訊」改好了：名稱跟著換、縮圖重讀（可能換了封面），人物、標籤重讀
pub fn edited(app: &mut UiApp, id: &str, title: &str) {
    let Some(d) = app.jf_browse.as_mut() else { return };
    if !title.trim().is_empty() {
        d.each_mut(id, |it| it.name = title.trim().to_string());
    }
    d.posters.remove(id);
    load_facets(app);
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
        "會從 Jellyfin 和 NAS 上刪掉，不會進資源回收筒，刪了不能復原。這台電腦上的檔案不會動。",
        "This deletes them from Jellyfin and the NAS. They don't go to a Recycle Bin and can't be undone. Files on this computer aren't touched."
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
    posters: Vec<String>,
}

/// 畫一組縮圖格（rows：最多幾列，None = 全部）
fn cells(ui: &mut egui::Ui, d: &JfBrowse, items: &[JfItem], rows: Option<usize>, out: &mut Picked) {
    let p = theme::pal(ui);
    let gap = 12.0;
    let cols = ((ui.available_width() + gap) / (CELL_W + gap)).floor().max(1.0) as usize;
    let cell_w = (ui.available_width() - gap * (cols as f32 - 1.0)) / cols as f32;
    let img_h = (cell_w * 9.0 / 16.0).round();
    let cell_h = img_h + 46.0;
    let clip = ui.clip_rect();
    let picking = !d.selected.is_empty();
    let ctrl = ui.input(|i| i.modifiers.command);
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
                // 已經在勾選、或按著 Ctrl：點影片是勾選；不然是播放（資料夾是進去）
                if video && (picking || ctrl) {
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

/// 區塊標題（「繼續觀看」等）
fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(2.0);
    ui.label(RichText::new(title).font(theme::font_bold(14.0)));
    ui.add_space(4.0);
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut d) = app.jf_browse.take() else { return };
    let mut close = false;
    let mut reload = false;
    let mut picked = Picked::default();
    let mut bulk_download = false;
    let mut bulk_delete = false;
    let mut find_dupes = false;
    let modal = egui::Modal::new(Id::new("jellyfin-browse")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        let p = theme::pal(ui);
        let w = (ctx.content_rect().width() - 80.0).clamp(420.0, 1040.0);
        let h = (ctx.content_rect().height() - 120.0).clamp(320.0, 760.0);
        ui.set_width(w);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Jellyfin").font(theme::font_bold(17.0)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if Btn::icon_only(Icon::Close).ghost().small().tooltip(tr!("關閉（Esc）", "Close (Esc)")).show(ui).clicked() {
                    close = true;
                }
                // 搜尋整個伺服器
                let r = ui.add(egui::TextEdit::singleline(&mut d.search).desired_width(220.0).hint_text(tr!("搜尋 Jellyfin 上的影片", "Search videos on Jellyfin")));
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && d.search.trim() != d.searched {
                    d.page = 0;
                    reload = true;
                }
                if !d.searched.is_empty() && Btn::new(tr!("清除搜尋", "Clear search")).small().ghost().show(ui).clicked() {
                    d.search.clear();
                    d.page = 0;
                    reload = true;
                }
            });
        });
        // 目前位置：媒體庫 › family › 2024，點前面的回到那一層；資料夾裡右邊可以排序、篩選
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = vec2(2.0, 4.0);
            if !d.searched.is_empty() {
                ui.label(RichText::new(trf!("搜尋「{}」", "Search “{}”", d.searched)).color(p.muted));
                return;
            }
            let top = d.path.is_empty() && d.facet.is_none();
            if Btn::new(tr!("媒體庫", "Libraries")).icon(Icon::Folder).small().ghost().selected(top).show(ui).clicked() && !top {
                d.path.clear();
                d.facet = None;
                d.page = 0;
                reload = true;
            }
            // 依人物、標籤、年份找的：媒體庫 › 人物：媽媽
            if let Some(f) = d.facet.clone() {
                ui.label(theme::muted(ui, "›"));
                Btn::new(f.label()).small().ghost().selected(true).show(ui);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    theme::field_style(ui);
                    let sort = d.sort;
                    egui::ComboBox::from_id_salt("jf-sort-facet").selected_text(d.sort.label()).width(130.0).show_ui(ui, |ui| {
                        for s in JfSort::ALL {
                            ui.selectable_value(&mut d.sort, s, s.label());
                        }
                    });
                    if sort != d.sort {
                        d.page = 0;
                        reload = true;
                    }
                });
                return;
            }
            let n = d.path.len();
            for i in 0..n {
                ui.label(theme::muted(ui, "›"));
                let last = i + 1 == n;
                if Btn::new(&d.path[i].1).small().ghost().selected(last).show(ui).clicked() && !last {
                    d.path.truncate(i + 1);
                    d.page = 0;
                    reload = true;
                }
            }
            if n == 0 {
                // 最上層：依人物、標籤、年份找（找整個伺服器）
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let mut pick = None;
                    let menu = |ui: &mut egui::Ui, label: &str, empty: &str, items: Vec<(String, JfFacet)>, pick: &mut Option<JfFacet>| {
                        let b = Btn::new(label).menu().small().ghost().enabled(!items.is_empty()).tooltip(if items.is_empty() { empty } else { "" }).show(ui);
                        egui::Popup::menu(&b).show(|ui| {
                            ui.set_min_width(180.0);
                            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                                for (text, f) in items {
                                    if ui.button(text).clicked() {
                                        *pick = Some(f);
                                    }
                                }
                            });
                        });
                    };
                    let years = d.years.iter().map(|y| (y.to_string(), JfFacet::Year(*y))).collect();
                    menu(ui, tr!("年份", "Year"), tr!("影片還沒有設年份", "No videos have a year yet"), years, &mut pick);
                    let tags = d.tags.iter().map(|t| (t.clone(), JfFacet::Tag(t.clone()))).collect();
                    menu(ui, tr!("標籤", "Tag"), tr!("影片還沒有標籤", "No videos have tags yet"), tags, &mut pick);
                    let people = d.persons.iter().map(|(id, n)| (n.clone(), JfFacet::Person(id.clone(), n.clone()))).collect();
                    menu(ui, tr!("人物", "People"), tr!("影片還沒有設人物", "No videos have people yet"), people, &mut pick);
                    ui.label(theme::muted(ui, tr!("依這些找：", "Find by:")));
                    ui.add_space(8.0);
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
                        find_dupes = true;
                    }
                    if let Some(f) = pick {
                        d.facet = Some(f);
                        d.filter = JfFilter::All;
                        d.page = 0;
                        reload = true;
                    }
                });
                return;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                theme::field_style(ui);
                let (sort, filter) = (d.sort, d.filter);
                egui::ComboBox::from_id_salt("jf-sort").selected_text(d.sort.label()).width(130.0).show_ui(ui, |ui| {
                    for s in JfSort::ALL {
                        ui.selectable_value(&mut d.sort, s, s.label());
                    }
                });
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
                if (sort, filter) != (d.sort, d.filter) {
                    d.page = 0;
                    reload = true;
                }
            });
        });
        ui.add_space(6.0);
        let body_h = h - 110.0 - if d.selected.is_empty() { 0.0 } else { 40.0 };
        match (&d.items, &d.error) {
            (_, Some(e)) => {
                ui.allocate_ui(vec2(w, body_h), |ui| {
                    ui.label(RichText::new(trf!("讀不到 Jellyfin：{e}", "Couldn't reach Jellyfin: {e}")).color(p.warn));
                    if Btn::new(tr!("再試一次", "Try again")).small().show(ui).clicked() {
                        reload = true;
                    }
                });
            }
            (None, None) => {
                ui.allocate_ui(vec2(w, body_h), |ui| {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tr!("正在讀取…", "Loading…"));
                    });
                });
            }
            (Some(items), None) if items.is_empty() => {
                ui.allocate_ui(vec2(w, body_h), |ui| {
                    let msg = if d.filter == JfFilter::All { tr!("這裡沒有影片", "No videos here") } else { tr!("這裡沒有符合的影片", "No matching videos here") };
                    ui.label(RichText::new(msg).color(p.muted));
                });
            }
            (Some(items), None) => {
                egui::ScrollArea::vertical().max_height(body_h).min_scrolled_height(body_h).auto_shrink([false, false]).show(ui, |ui| {
                    if d.at_top() {
                        // 最上層：繼續觀看、最近加入各一列，接著是媒體庫
                        for (title, list, salt) in [(tr!("繼續觀看", "Continue watching"), &d.resume, "resume"), (tr!("最近加入", "Recently added"), &d.latest, "latest")] {
                            if !list.is_empty() {
                                section(ui, title);
                                ui.push_id(salt, |ui| cells(ui, &d, list, Some(1), &mut picked));
                            }
                        }
                        if !d.resume.is_empty() || !d.latest.is_empty() {
                            section(ui, tr!("媒體庫", "Libraries"));
                        }
                    }
                    ui.push_id("items", |ui| cells(ui, &d, items, None, &mut picked));
                });
            }
        }
        // 勾選了：下面一排一次下載、刪除
        if !d.selected.is_empty() {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(trf!("已勾選 {} 支", "{} selected", d.selected.len())).font(theme::font_bold(13.0)));
                if Btn::new(tr!("下載到這台電腦", "Download to this computer")).icon(Icon::Download).small().primary().show(ui).clicked() {
                    bulk_download = true;
                }
                if Btn::new(tr!("從 Jellyfin 刪除", "Delete from Jellyfin")).icon(Icon::Trash).small().danger().show(ui).clicked() {
                    bulk_delete = true;
                }
                if Btn::new(tr!("取消勾選", "Clear selection")).small().ghost().show(ui).clicked() {
                    d.selected.clear();
                }
            });
        }
        // 換頁
        let pages = d.total.div_ceil(PAGE).max(1);
        if !d.at_top() && pages > 1 {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if Btn::icon_only(Icon::ChevR).small().ghost().enabled(d.page + 1 < pages).show(ui).clicked() {
                        d.page += 1;
                        reload = true;
                    }
                    ui.label(trf!("第 {} / {} 頁", "Page {} / {}", d.page + 1, pages));
                    if Btn::icon_only(Icon::ChevL).small().ghost().enabled(d.page > 0).show(ui).clicked() {
                        d.page -= 1;
                        reload = true;
                    }
                    ui.label(RichText::new(trf!("共 {} 個", "{} in all", d.total)).color(p.muted));
                });
            });
        }
    });
    // 檢視器開著時 Esc 是關檢視器，不要連瀏覽視窗一起關；有勾選時 Esc 先取消勾選
    if close || (modal.should_close() && app.viewer.is_none() && app.ask.is_none() && app.jf_edit.is_none() && app.jf_dupes.is_none()) {
        if !close && !d.selected.is_empty() {
            d.selected.clear();
            app.jf_browse = Some(d);
        }
        return;
    }
    if let Some(it) = picked.toggle {
        if d.selected.remove(&it.id).is_none() {
            d.selected.insert(it.id.clone(), it);
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
        let (user, people, tags) = (d.user.clone().unwrap_or_default(), d.persons.iter().map(|(_, n)| n.clone()).collect(), d.tags.clone());
        app.jf_browse = Some(d);
        return super::jellyfin_edit::open(app, it.id, user, it.name, people, tags);
    }
    if let Some(it) = picked.open {
        match it.kind {
            JfKind::Folder => {
                d.facet = None;
                d.path.push((it.id.clone(), it.name.clone()));
                d.search.clear();
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
