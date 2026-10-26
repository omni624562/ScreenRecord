//! 在檔案庫的 Jellyfin 分頁直接改影片資訊：家人用手機傳上去的影片不用先下載，就能改標題、說明、人物、標籤、年份、封面。
//! 存好之後重新讀回來比對，Jellyfin 沒有改到的欄位直接講出來（不會默默沒存到）。
//! 見 core::jellyfin 的 item_info、set_info、set_cover、unsaved

use super::theme::{self, Btn, Icon};
use super::UiApp;
use eframe::egui::{self, vec2, Align, Id, Layout, Rect, RichText, Sense, Stroke};
use screenrecorder_core::jellyfin::{self, JfInfo, NameKind};
use screenrecorder_core::{tr, trf};
use std::sync::{Arc, Mutex};

/// 封面預覽的大小（16:9）
const COVER: egui::Vec2 = vec2(192.0, 108.0);

pub struct JfEdit {
    id: String,
    user: String,
    name: String,
    /// 讀到的（判斷有沒有改過）；還在讀時是 None
    orig: Option<JfInfo>,
    info: JfInfo,
    year: String,
    /// 人物、標籤輸入框裡還沒變成小標籤的字（儲存時一起存）
    new_person: String,
    new_tag: String,
    /// 伺服器上已經有的人物、標籤與用在幾支影片（按一下加入；次數讀好之前是 0）
    known_people: Vec<(String, usize)>,
    known_tags: Vec<(String, usize)>,
    /// 「全部 N 個 ▾」清單（人物、標籤各一個）
    pick_people: Picker,
    pick_tags: Picker,
    /// 目前的封面（預覽用）
    poster: Option<egui::TextureHandle>,
    /// 選好的新封面
    cover: Option<Picked>,
    /// 存檔時移除 Jellyfin 上的封面
    remove_cover: bool,
    saving: bool,
    /// 按下儲存的時間（等太久時說明伺服器還在存）
    saving_since: Option<std::time::Instant>,
    error: Option<String>,
    /// 改過了又按 Esc、點外面：不關，提示要放棄請按「取消」
    keep_hint: bool,
}

/// 開啟（背景讀這支影片目前的資訊和封面）
pub fn open(app: &mut UiApp, id: String, user: String, name: String, known_people: Vec<String>, known_tags: Vec<String>) {
    let Some(jf) = app.jellyfin() else { return };
    app.jf_edit = Some(JfEdit {
        id: id.clone(),
        user: user.clone(),
        name,
        orig: None,
        info: JfInfo::default(),
        year: String::new(),
        new_person: String::new(),
        new_tag: String::new(),
        known_people: known_people.into_iter().map(|n| (n, 0)).collect(),
        known_tags: known_tags.into_iter().map(|n| (n, 0)).collect(),
        pick_people: Picker::default(),
        pick_tags: Picker::default(),
        poster: None,
        cover: None,
        remove_cover: false,
        saving: false,
        saving_since: None,
        keep_hint: false,
        error: None,
    });
    load_usage(app);
    let (key, jf2, id2) = (id.clone(), jf.clone(), id.clone());
    app.spawn(async move { tokio::task::spawn_blocking(move || jf.item_info(&user, &key)).await.unwrap_or_else(|e| Err(e.to_string())) }, move |app, r| {
        let Some(d) = app.jf_edit.as_mut().filter(|d| d.id == id) else { return };
        match r {
            Ok(info) => {
                d.year = info.year.map(|y| y.to_string()).unwrap_or_default();
                d.info = info.clone();
                d.orig = Some(info);
            }
            Err(e) => d.error = Some(trf!("讀不到 Jellyfin 上的影片資訊：{e}", "Couldn't read the details from Jellyfin: {e}")),
        }
    });
    let ctx = app.ctx.clone();
    app.spawn(
        async move {
            let key = id2.clone();
            let img = tokio::task::spawn_blocking(move || jf2.poster(&key).and_then(|b| image_of(&b))).await.ok().flatten();
            (id2, img.map(|i| ctx.load_texture("jf-edit-poster", i, egui::TextureOptions::LINEAR)))
        },
        |app, (id, tex)| {
            if let Some(d) = app.jf_edit.as_mut().filter(|d| d.id == id) {
                d.poster = tex;
            }
        },
    );
}

/// 讀全部影片用到的人物、標籤與次數（常用依次數排）；讀不到就維持原本的清單
fn load_usage(app: &mut UiApp) {
    let Some(jf) = app.jellyfin() else { return };
    let Some((id, user)) = app.jf_edit.as_ref().map(|d| (d.id.clone(), d.user.clone())) else { return };
    app.spawn(async move { tokio::task::spawn_blocking(move || jf.usage(&user)).await.unwrap_or_else(|e| Err(e.to_string())) }, move |app, r| {
        let Some(d) = app.jf_edit.as_mut().filter(|d| d.id == id) else { return };
        match r {
            Ok(u) => {
                d.known_people = merge_usage(u.people, &d.known_people);
                d.known_tags = merge_usage(u.tags, &d.known_tags);
            }
            Err(e) => screenrecorder_core::info!("[Jellyfin] 讀不到人物、標籤的使用次數：{e}"),
        }
    });
}

/// 背景把全部影片裡的 from 改成 to；改好通知，常用、「依這些找」的清單重讀，
/// 這台電腦記的釘選、最近用過與開著的影片資訊裡的也跟著換
pub(super) fn start_rename(app: &mut UiApp, user: String, kind: NameKind, from: String, to: String) {
    let Some(jf) = app.jellyfin() else { return };
    let progress = Arc::new(Mutex::new((0usize, 0usize)));
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    app.jf_rename = Some(Renaming { kind, from: from.clone(), to: to.clone(), progress: progress.clone(), cancel: cancel.clone() });
    let ctx = app.ctx.clone();
    let (f2, t2) = (from.clone(), to.clone());
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || {
                let r = jf.rename_name(&user, kind, &f2, &t2, &|done, total| {
                    *progress.lock().unwrap_or_else(|e| e.into_inner()) = (done, total);
                    ctx.request_repaint();
                    !cancel.load(std::sync::atomic::Ordering::Relaxed)
                });
                (r, cancel.load(std::sync::atomic::Ordering::Relaxed))
            })
            .await
            .unwrap_or_else(|e| (Err(e.to_string()), false))
        },
        move |app, (r, stopped)| {
            app.jf_rename = None;
            let what = if kind == NameKind::Person { tr!("人物", "person") } else { tr!("標籤", "tag") };
            match r {
                Ok(n) if stopped => app.toast(
                    trf!("已停止：{n} 支改成「{to}」，其他還是「{from}」（再改一次會接著改完）", "Stopped: {n} videos now use “{to}”; the rest still use “{from}” (rename again to finish)"),
                    false,
                ),
                Ok(n) => app.toast(trf!("✓ {n} 支影片的{what}「{from}」改成「{to}」", "✓ Changed {what} “{from}” to “{to}” in {n} videos"), false),
                Err(e) => app.toast(trf!("{what}「{from}」沒有全部改好：{e}", "Couldn't change every “{from}”: {e}"), true),
            }
            let key = jellyfin::name_key(&from);
            let swap = |list: &mut Vec<String>| {
                if list.iter().any(|x| jellyfin::name_key(x) == key) {
                    for x in list.iter_mut().filter(|x| jellyfin::name_key(x) == key) {
                        *x = to.clone();
                    }
                    let mut seen = std::collections::HashSet::new();
                    list.retain(|x| seen.insert(jellyfin::name_key(x)));
                    true
                } else {
                    false
                }
            };
            let s = &mut app.s;
            let (pins, recent) = if kind == NameKind::Person { (&mut s.jf_pin_people, &mut s.jf_recent_people) } else { (&mut s.jf_pin_tags, &mut s.jf_recent_tags) };
            let changed = swap(pins) | swap(recent);
            if changed {
                app.save_settings();
            }
            if let Some(d) = app.jf_edit.as_mut() {
                fn pick(i: &mut JfInfo, kind: NameKind) -> &mut Vec<String> {
                    if kind == NameKind::Person {
                        &mut i.people
                    } else {
                        &mut i.tags
                    }
                }
                swap(pick(&mut d.info, kind));
                if let Some(o) = d.orig.as_mut() {
                    swap(pick(o, kind));
                }
                let known = if kind == NameKind::Person { &mut d.known_people } else { &mut d.known_tags };
                known.retain(|(n, _)| jellyfin::name_key(n) != key);
            }
            load_usage(app);
            super::jellyfin_browse::reload_facets(app);
            // 本機同名的影片也跟著改
            app.sync_details_all(true);
        },
    );
}

/// 背景從全部影片移除一個人物或標籤（「依這些找」清單按右鍵「從全部影片移除」）；改好通知，清單重讀，
/// 這台電腦記的釘選、最近用過與開著的影片資訊裡的也拿掉
pub(super) fn start_remove(app: &mut UiApp, user: String, kind: NameKind, name: String) {
    let Some(jf) = app.jellyfin() else { return };
    let progress = Arc::new(Mutex::new((0usize, 0usize)));
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    app.jf_rename = Some(Renaming { kind, from: name.clone(), to: String::new(), progress: progress.clone(), cancel: cancel.clone() });
    let ctx = app.ctx.clone();
    let n2 = name.clone();
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || {
                let r = jf.remove_name(&user, kind, &n2, &|done, total| {
                    *progress.lock().unwrap_or_else(|e| e.into_inner()) = (done, total);
                    ctx.request_repaint();
                    !cancel.load(std::sync::atomic::Ordering::Relaxed)
                });
                (r, cancel.load(std::sync::atomic::Ordering::Relaxed))
            })
            .await
            .unwrap_or_else(|e| (Err(e.to_string()), false))
        },
        move |app, (r, stopped)| {
            app.jf_rename = None;
            let what = if kind == NameKind::Person { tr!("人物", "person") } else { tr!("標籤", "tag") };
            match r {
                Ok(n) if stopped => app.toast(trf!("已停止：{n} 支拿掉了「{name}」（再移除一次會接著做完）", "Stopped: removed “{name}” from {n} videos (remove again to finish)"), false),
                Ok(n) => app.toast(trf!("✓ {n} 支影片拿掉了{what}「{name}」", "✓ Removed {what} “{name}” from {n} videos"), false),
                Err(e) => app.toast(trf!("{what}「{name}」沒有全部拿掉：{e}", "Couldn't remove every “{name}”: {e}"), true),
            }
            let key = jellyfin::name_key(&name);
            let drop = |list: &mut Vec<String>| {
                let before = list.len();
                list.retain(|x| jellyfin::name_key(x) != key);
                list.len() != before
            };
            let s = &mut app.s;
            let (pins, recent) = if kind == NameKind::Person { (&mut s.jf_pin_people, &mut s.jf_recent_people) } else { (&mut s.jf_pin_tags, &mut s.jf_recent_tags) };
            if drop(pins) | drop(recent) {
                app.save_settings();
            }
            if let Some(d) = app.jf_edit.as_mut() {
                for i in [Some(&mut d.info), d.orig.as_mut()].into_iter().flatten() {
                    drop(if kind == NameKind::Person { &mut i.people } else { &mut i.tags });
                }
                let known = if kind == NameKind::Person { &mut d.known_people } else { &mut d.known_tags };
                known.retain(|(n, _)| jellyfin::name_key(n) != key);
            }
            load_usage(app);
            super::jellyfin_browse::reload_facets(app);
            // 本機同名的影片也跟著改
            app.sync_details_all(true);
        },
    );
}

/// 這一類正在背景重新命名的（清單上畫進度）
pub(super) fn rename_busy(app: &UiApp, kind: NameKind) -> Option<Busy> {
    app.jf_rename.as_ref().filter(|r| r.kind == kind).map(|r| {
        let (done, total) = *r.progress.lock().unwrap_or_else(|e| e.into_inner());
        Busy { cancel: r.cancel.clone(), from: r.from.clone(), to: r.to.clone(), done, total }
    })
}

/// 使用次數（多的在前）加上伺服器上有、但還沒有影片用到的（次數 0）
pub(super) fn merge_usage(mut used: Vec<(String, usize)>, known: &[(String, usize)]) -> Vec<(String, usize)> {
    for (n, _) in known {
        if !used.iter().any(|(u, _)| jellyfin::name_key(u) == jellyfin::name_key(n)) {
            used.push((n.clone(), 0));
        }
    }
    used
}

/// 「全部 N 個 ▾」清單的狀態：搜尋的字、排序、勾了哪些、正在打的重新命名
#[derive(Default)]
pub(super) struct Picker {
    search: String,
    sort: Sort,
    checked: Vec<String>,
    rename: Option<RenameForm>,
    /// 按了「改 N 支」：（舊的, 新的寫法），show 讀走後開始改
    start: Option<(String, String)>,
    /// 下一次畫的時候開啟「全部 N 個」清單（常用按右鍵選「重新命名…」）
    open: bool,
}

impl Picker {
    /// 按了「改 N 支」：（舊的, 新的寫法），讀走後清掉
    pub(super) fn take_rename(&mut self) -> Option<(String, String)> {
        self.start.take()
    }
}

/// 重新命名的框：哪一個（用在幾支）、新的寫法
struct RenameForm {
    from: String,
    count: usize,
    to: String,
    /// 剛開啟：游標放進輸入框
    focus: bool,
}

impl RenameForm {
    /// 有看起來一樣、寫法不同的（用最多的那個）時先填它，不然填原本的
    fn new(from: &str, count: usize, all: &[(String, usize)]) -> Self {
        let twin = all.iter().filter(|(m, _)| jellyfin::spelling_diff(from, m).is_some()).max_by_key(|(_, c)| *c).map(|(m, _)| m.clone());
        Self { from: from.to_string(), count, to: twin.unwrap_or_else(|| from.to_string()), focus: true }
    }
}

/// 背景正在把一個人物或標籤重新命名（改的時候可以關掉影片資訊，會繼續）
pub struct Renaming {
    kind: NameKind,
    from: String,
    to: String,
    /// （改好幾支, 全部幾支）
    progress: Arc<Mutex<(usize, usize)>>,
    /// 按了「停止」：改完正在改的那一支就停
    cancel: Arc<std::sync::atomic::AtomicBool>,
}

/// 清單上畫進度用：正在改的（舊的, 新的, 改好幾支, 全部幾支）
#[derive(Clone)]
pub(super) struct Busy {
    cancel: Arc<std::sync::atomic::AtomicBool>,
    from: String,
    to: String,
    done: usize,
    total: usize,
}

#[derive(Default, Clone, Copy, PartialEq)]
enum Sort {
    #[default]
    Count,
    Name,
    Recent,
}

/// 最近加過的放到最前面（最多 RECENT_GENRES 個）
pub(super) fn remember(recent: &mut Vec<String>, names: &[String]) {
    for n in names.iter().rev() {
        recent.retain(|r| jellyfin::name_key(r) != jellyfin::name_key(n));
        recent.insert(0, n.clone());
    }
    recent.truncate(super::settings::RECENT_GENRES);
}

/// 圖片檔（JPG、PNG…）轉成可以畫的圖
fn image_of(bytes: &[u8]) -> Option<egui::ColorImage> {
    let pm = screenrecorder_core::picture::decode_pixmap(bytes)?;
    Some(egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data()))
}

/// 輸入框裡的字變成小標籤（可以一次打好幾個，用逗號或頓號分開）；已經有的不重複加
pub(super) fn commit(list: &mut Vec<String>, input: &mut String) {
    for v in input.split([',', '，', '、', ';', '；']).map(str::trim).filter(|v| !v.is_empty()) {
        if !list.iter().any(|x| x.to_lowercase() == v.to_lowercase()) {
            list.push(v.to_string());
        }
    }
    input.clear();
}

/// 欄位上面的小標題
pub(super) fn label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).font(theme::font_bold(12.5)).color(theme::pal(ui).muted));
}

/// 一個像輸入框的方框：裡面是已經加的小標籤（點 × 移除）和打字的地方（Enter、逗號加入；空的時按退格移除最後一個）。
/// 下面一排是伺服器上已經有的，按一下加入
pub(super) fn chips(ui: &mut egui::Ui, id: &str, list: &mut Vec<String>, input: &mut String, known: &[(String, usize)], prefs: Prefs, hint: &str) {
    let p = theme::pal(ui);
    let mut remove = None;
    // 點了名字（不是 ×）：拿回輸入框改
    let mut edit = None;
    let focus_id = Id::new(("jf-edit-chips", id));
    let focused = ui.memory(|m| m.has_focus(focus_id));
    egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, if focused { p.accent } else { p.border })).corner_radius(6.0).inner_margin(egui::Margin::symmetric(6, 5)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            for (i, v) in list.iter().enumerate() {
                let r = theme::chip(ui, &format!("{v}  ×"), theme::Tone::Accent, true).on_hover_text(tr!("點名字修改，點 × 移除", "Click the name to edit it, × to remove it"));
                if r.clicked() {
                    // 右邊的 × 是移除，其他地方是修改
                    let on_x = r.interact_pointer_pos().is_some_and(|pos| pos.x > r.rect.right() - 20.0);
                    if on_x {
                        remove = Some(i);
                    } else {
                        edit = Some(i);
                    }
                }
            }
            let w = (ui.available_size_before_wrap().x - 12.0).max(120.0);
            let r = ui.add(egui::TextEdit::singleline(input).id(focus_id).frame(egui::Frame::NONE).desired_width(w).hint_text(if list.is_empty() { hint } else { "" }));
            let (enter, back) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Backspace)));
            if r.changed() && input.contains([',', '，', '、', ';', '；']) {
                commit(list, input);
            }
            if r.lost_focus() && enter && !input.trim().is_empty() {
                commit(list, input);
                r.request_focus();
            }
            if r.has_focus() && back && input.is_empty() && !list.is_empty() && !r.changed() {
                remove = Some(list.len() - 1);
            }
        });
    });
    if let Some(i) = remove {
        list.remove(i);
    }
    // 修改：拿回輸入框（輸入框裡原本打的字先加進去），改好按 Enter
    if let Some(i) = edit {
        commit(list, input);
        let i = i.min(list.len().saturating_sub(1));
        if i < list.len() {
            *input = list.remove(i);
            ui.memory_mut(|m| m.request_focus(focus_id));
        }
    }
    suggestions(ui, id, list, input, known, prefs);
}

/// 這台電腦記住的：釘選的（可以改）、最近加過的
pub(super) struct Prefs<'a> {
    pub(super) pins: &'a mut Vec<String>,
    pub(super) recent: &'a [String],
    /// 改了釘選（要存設定）
    pub(super) changed: &'a mut bool,
    /// 「全部 N 個 ▾」清單的狀態
    pub(super) picker: &'a mut Picker,
    /// 這一類正在重新命名的
    pub(super) busy: Option<Busy>,
}

/// 輸入框下面：沒打字時一行「常用」（釘選的在前，再依使用次數；放得下幾個就幾個），最後是「全部 N 個 ▾」清單；
/// 打字時列出全部符合的
fn suggestions(ui: &mut egui::Ui, id: &str, list: &mut Vec<String>, input: &mut String, known: &[(String, usize)], mut prefs: Prefs) {
    let p = theme::pal(ui);
    let key = jellyfin::name_key;
    // 候選：伺服器上有的＋釘選的（伺服器上已經沒有的也列），已經加的不列
    let mut cands: Vec<(String, usize)> = known.to_vec();
    for pin in prefs.pins.iter() {
        if !cands.iter().any(|(n, _)| key(n) == key(pin)) {
            cands.push((pin.clone(), 0));
        }
    }
    cands.retain(|(n, _)| !list.iter().any(|x| key(x) == key(n)));
    let pins_now = prefs.pins.clone();
    let pin_rank = |n: &str| pins_now.iter().position(|p| key(p) == key(n));
    // 釘選的在前（照釘選的順序），其他依次數（known 已經是次數多的在前）
    let mut row: Vec<&(String, usize)> = cands.iter().collect();
    row.sort_by_key(|(n, _)| pin_rank(n).unwrap_or(usize::MAX));
    let typed = input.trim().to_lowercase();
    let mut add: Vec<String> = vec![];
    if !typed.is_empty() {
        let matches: Vec<&&(String, usize)> = row.iter().filter(|(n, _)| n.to_lowercase().contains(&typed) || key(n).contains(&key(&typed))).collect();
        if matches.is_empty() {
            if !known.is_empty() {
                ui.label(RichText::new(tr!("伺服器上沒有符合的，按 Enter 新增", "No match on the server; press Enter to add it")).font(theme::font(12.0)).color(p.muted));
            }
            return;
        }
        ui.add_space(2.0);
        egui::ScrollArea::vertical().id_salt(("jf-edit-known", id)).max_height(150.0).auto_shrink([false, true]).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                ui.label(RichText::new(tr!("符合的：", "Matches:")).font(theme::font(12.0)).color(p.muted));
                for (n, c) in matches {
                    if sug_chip(ui, n, *c, pin_rank(n).is_some()).clicked() {
                        add.push(n.clone());
                    }
                }
            });
        });
    } else if !cands.is_empty() {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(RichText::new(tr!("常用：", "Used before:")).font(theme::font(12.0)).color(p.muted));
            let all = Btn::new(trf!("全部 {} 個", "All {}", cands.len())).small().menu().tooltip(tr!("搜尋、勾選多個一次加入、釘選常用的", "Search, add several at once, pin favorites"));
            let all_w = all.width(ui) + 6.0;
            // 一行放得下的才放，「全部」那一顆一定留位置；按右鍵可以重新命名、釘選
            let mut toggle_pin = None;
            for (n, c) in &row {
                if sug_width(ui, n, *c, pin_rank(n).is_some()) + 6.0 > ui.available_width() - all_w {
                    break;
                }
                let r = sug_chip(ui, n, *c, pin_rank(n).is_some());
                if r.clicked() {
                    add.push(n.clone());
                }
                r.context_menu(|ui| {
                    if ui.button(tr!("加入", "Add")).clicked() {
                        add.push(n.clone());
                    }
                    if *c > 0
                        && prefs.busy.is_none()
                        && ui
                            .button(tr!("重新命名…", "Rename…"))
                            .on_hover_text(tr!("寫錯了？改成正確的寫法（改成已經有的就是合併）", "Misspelled? Change it to the right spelling (choosing an existing one merges them)"))
                            .clicked()
                    {
                        prefs.picker.rename = Some(RenameForm::new(n, *c, known));
                        prefs.picker.search.clear();
                        prefs.picker.open = true;
                    }
                    if ui.button(if pin_rank(n).is_some() { tr!("取消釘選", "Unpin") } else { tr!("釘選", "Pin") }).clicked() {
                        toggle_pin = Some(n.clone());
                    }
                });
            }
            let resp = all.show(ui);
            if std::mem::take(&mut prefs.picker.open) {
                egui::Popup::open_id(ui.ctx(), egui::Popup::default_response_id(&resp));
            }
            egui::Popup::menu(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                if let Some(picked) = picker_body(ui, id, &cands, known, &mut prefs) {
                    add.extend(picked);
                    ui.close();
                }
            });
            if let Some(n) = toggle_pin {
                if prefs.pins.iter().any(|x| key(x) == key(&n)) {
                    prefs.pins.retain(|x| key(x) != key(&n));
                } else {
                    prefs.pins.push(n);
                }
                *prefs.changed = true;
            }
        });
    }
    if !add.is_empty() {
        for n in add {
            if !list.iter().any(|x| key(x) == key(&n)) {
                list.push(n);
            }
        }
        // 打了一半的字是用來找的：加好就清掉
        input.clear();
    }
}

/// 常用的一顆：「＋ 名稱」（釘選的「★ 名稱」），後面小小的灰字是用在幾支影片
fn sug_width(ui: &egui::Ui, name: &str, count: usize, pinned: bool) -> f32 {
    let p = theme::pal(ui);
    let t = ui.painter().layout_no_wrap(if pinned { format!("  {name}") } else { format!("＋ {name}") }, theme::font(12.5), p.text).size().x + if pinned { 8.0 } else { 0.0 };
    let c = if count > 0 { ui.painter().layout_no_wrap(count.to_string(), theme::font(12.0), p.muted).size().x + 5.0 } else { 0.0 };
    t + c + 20.0
}

fn sug_chip(ui: &mut egui::Ui, name: &str, count: usize, pinned: bool) -> egui::Response {
    let p = theme::pal(ui);
    let w = sug_width(ui, name, count, pinned);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 24.0), Sense::click());
    let fill = if pinned { p.warn_soft } else { p.surface2 };
    ui.painter().rect_filled(rect, 12.0, if resp.hovered() { fill.gamma_multiply(1.15) } else { fill });
    // 釘選的前面是星星（畫的圖示），其他是「＋」
    let mut x = rect.left() + 10.0;
    let t = if pinned {
        theme::paint_icon(ui.painter(), Rect::from_center_size(egui::pos2(x + 6.0, rect.center().y), vec2(12.0, 12.0)), Icon::Star, p.warn);
        x += 8.0;
        ui.painter().layout_no_wrap(format!("  {name}"), theme::font(12.5), p.text)
    } else {
        ui.painter().layout_no_wrap(format!("＋ {name}"), theme::font(12.5), p.text)
    };
    let tw = t.size().x + x - rect.left() - 10.0;
    ui.painter().galley(egui::pos2(x, rect.center().y - t.size().y / 2.0), t, p.text);
    if count > 0 {
        let c = ui.painter().layout_no_wrap(count.to_string(), theme::font(12.0), p.muted);
        ui.painter().galley(egui::pos2(rect.left() + 15.0 + tw, rect.center().y - c.size().y / 2.0), c, p.muted);
    }
    let tip = if count > 0 { trf!("按一下加入（用在 {count} 支影片）", "Click to add (used in {count} videos)") } else { tr!("按一下加入", "Click to add").to_string() };
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip)
}

/// 「全部 N 個 ▾」清單：搜尋、排序（次數 / 名稱 / 最近用過）、☆ 釘選、勾選多個一次加入、重新命名（寫錯的改成正確的寫法）；
/// 回傳要加入的
/// cands 是可以加的（這支影片還沒有的），all 是伺服器上全部的（重新命名時找相近的、看起來一樣的用）
fn picker_body(ui: &mut egui::Ui, id: &str, cands: &[(String, usize)], all: &[(String, usize)], prefs: &mut Prefs) -> Option<Vec<String>> {
    let p = theme::pal(ui);
    let key = jellyfin::name_key;
    // 寬度固定：清單在選單裡，靠右的東西不能讓它每一格越撐越寬
    const W: f32 = 330.0;
    ui.set_width(W);
    ui.set_max_width(W);
    let pk = &mut *prefs.picker;
    let hint = if id == "people" { tr!("搜尋人物…", "Search people…") } else { tr!("搜尋標籤…", "Search tags…") };
    let sr = ui.add(egui::TextEdit::singleline(&mut pk.search).hint_text(hint).desired_width(W).margin(egui::Margin { left: 4, right: 26, top: 2, bottom: 2 }));
    super::toolbar::clear_x(ui, &sr, &mut pk.search);
    ui.add_space(4.0);
    theme::segmented(ui, &mut pk.sort, &[(Sort::Count, tr!("依次數", "By count")), (Sort::Name, tr!("依名稱", "By name")), (Sort::Recent, tr!("最近用過", "Recent"))], true);
    ui.add_space(4.0);
    let q = pk.search.trim().to_lowercase();
    let mut items: Vec<&(String, usize)> = cands.iter().filter(|(n, _)| q.is_empty() || n.to_lowercase().contains(&q) || key(n).contains(&key(&q))).collect();
    match pk.sort {
        Sort::Count => {}
        Sort::Name => items.sort_by_key(|(n, _)| n.to_lowercase()),
        Sort::Recent => items.sort_by_key(|(n, _)| prefs.recent.iter().position(|r| key(r) == key(n)).unwrap_or(usize::MAX)),
    }
    let (pinned, others): (Vec<_>, Vec<_>) = items.into_iter().partition(|(n, _)| prefs.pins.iter().any(|x| key(x) == key(n)));
    // 看起來一樣、其實寫法不同的（多了看不見的字元）：標「寫法不同」
    let mut looks: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (n, _) in all {
        *looks.entry(jellyfin::look_key(n)).or_default() += 1;
    }
    let differs = |n: &str| -> Option<String> {
        if looks.get(&jellyfin::look_key(n)).copied().unwrap_or(0) < 2 {
            return None;
        }
        all.iter().find_map(|(m, _)| jellyfin::spelling_diff(n, m))
    };
    let busy = prefs.busy.clone();
    let mut toggle_pin: Option<String> = None;
    egui::ScrollArea::vertical().id_salt(("jf-edit-all", id)).max_height(300.0).show(ui, |ui| {
        if pinned.is_empty() && others.is_empty() {
            ui.label(RichText::new(tr!("沒有符合的", "No matches")).font(theme::font(12.5)).color(p.muted));
        }
        for (title, group) in [(tr!("釘選", "Pinned"), &pinned), (tr!("全部", "All"), &others)] {
            if group.is_empty() {
                continue;
            }
            if !pinned.is_empty() {
                ui.label(RichText::new(title).font(theme::font(12.0)).color(p.muted));
            }
            for (n, c) in group.iter() {
                // 正在重新命名的這一個：灰色「舊 → 新」、改好幾支和進度條
                if let Some(b) = busy.as_ref().filter(|b| key(&b.from) == key(n)) {
                    let (row, _) = ui.allocate_exact_size(vec2(W - 16.0, 26.0), Sense::hover());
                    ui.painter().text(
                        egui::pos2(row.min.x + 28.0, row.center().y),
                        egui::Align2::LEFT_CENTER,
                        if b.to.is_empty() { trf!("{n}：移除中", "{n}: removing") } else { format!("{n} → {}", b.to) },
                        theme::font(13.0),
                        p.muted,
                    );
                    // 右邊：改好幾支、停止
                    let stop = Rect::from_min_size(egui::pos2(row.max.x - 22.0, row.center().y - 10.0), vec2(20.0, 20.0));
                    let sr = ui.interact(stop, Id::new(("jf-rename-stop", n.as_str())), Sense::click()).on_hover_text(tr!("停止（改好的照樣保留）", "Stop (videos already changed stay changed)"));
                    theme::paint_icon(ui.painter(), stop.shrink(4.0), Icon::Stop, if sr.hovered() { p.text } else { p.muted });
                    if sr.clicked() {
                        b.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    ui.painter().text(egui::pos2(stop.min.x - 6.0, row.center().y), egui::Align2::RIGHT_CENTER, format!("{} / {}", b.done, b.total), theme::font(12.0), p.muted);
                    let (bar, _) = ui.allocate_exact_size(vec2(W - 16.0, 8.0), Sense::hover());
                    let bar = bar.shrink2(vec2(4.0, 1.0));
                    ui.painter().rect_filled(bar, 3.0, p.surface2);
                    let frac = if b.total > 0 { b.done as f32 / b.total as f32 } else { 0.0 };
                    ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * frac, bar.height())), 3.0, p.accent);
                    continue;
                }
                let is_pin = !std::ptr::eq(group, &others);
                // 每一列固定大小：星星、勾選框＋名稱、右邊的「寫法不同」「重新命名」與次數（畫上去，不用靠右的排版，選單才不會越撐越寬）
                let (row, _) = ui.allocate_exact_size(vec2(W - 16.0, 26.0), Sense::hover());
                let tip = if is_pin { tr!("取消釘選", "Unpin") } else { tr!("釘選：永遠排在常用的最前面", "Pin: always first in the list") };
                let sr = Rect::from_min_size(row.min + vec2(0.0, 2.0), vec2(22.0, 22.0));
                let star = ui.interact(sr, Id::new(("jf-pin", id, n.as_str())), Sense::click());
                if star.hovered() {
                    ui.painter().rect_filled(sr, 5.0, p.surface2);
                }
                theme::paint_icon(ui.painter(), sr.shrink(4.0), Icon::Star, if is_pin { p.warn } else { p.muted.gamma_multiply(0.6) });
                if star.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip).clicked() {
                    toggle_pin = Some(n.clone());
                }
                let cw = if *c > 0 { 52.0 } else { 0.0 };
                let mut right = row.max.x - cw;
                if *c > 0 {
                    ui.painter().text(egui::pos2(row.max.x - 4.0, row.center().y), egui::Align2::RIGHT_CENTER, trf!("{c} 支", "{c}"), theme::font(12.0), p.muted);
                }
                // 重新命名：滑鼠移到這一列才出現（伺服器上有影片用到的才能改；同時只改一個）
                let open = pk.rename.as_ref().is_some_and(|f| f.from == *n);
                if *c > 0 && busy.is_none() && (open || ui.rect_contains_pointer(row)) {
                    let b = Btn::new(tr!("重新命名", "Rename")).icon(Icon::Edit).small().ghost().height(22.0).selected(open).tooltip(tr!(
                        "寫錯了？改成正確的寫法（改成已經有的就是合併），Jellyfin 上用到的影片一起改",
                        "Misspelled? Change it to the right spelling (choosing an existing one merges them); videos on Jellyfin using it are updated"
                    ));
                    let bw = b.width(ui);
                    let br = Rect::from_min_max(egui::pos2(right - bw, row.center().y - 11.0), egui::pos2(right, row.center().y + 11.0));
                    if ui.scope_builder(egui::UiBuilder::new().max_rect(br), |ui| b.show(ui)).inner.clicked() {
                        pk.rename = if open { None } else { Some(RenameForm::new(n, *c, all)) };
                    }
                    right -= bw + 4.0;
                }
                if let Some(why) = differs(n) {
                    let g = ui.painter().layout_no_wrap(tr!("寫法不同", "Differs").to_string(), theme::font(12.0), p.warn);
                    let br = Rect::from_min_max(egui::pos2(right - g.size().x - 12.0, row.center().y - 9.0), egui::pos2(right, row.center().y + 9.0));
                    ui.painter().rect_filled(br, 9.0, p.warn_soft);
                    ui.painter().galley(br.center() - g.size() / 2.0, g, p.warn);
                    ui.interact(br, Id::new(("jf-differs", id, n.as_str())), Sense::hover()).on_hover_text(trf!(
                        "看起來和另一個一樣，但寫法不同：{why}。可以用「重新命名」合併",
                        "Looks the same as another one but is spelled differently: {why}. Use Rename to merge them"
                    ));
                    right = br.min.x - 4.0;
                }
                let mut on = pk.checked.iter().any(|x| key(x) == key(n));
                let cb = Rect::from_min_max(egui::pos2(sr.max.x + 6.0, row.min.y), egui::pos2(right, row.max.y));
                let clicked = ui.scope_builder(egui::UiBuilder::new().max_rect(cb).layout(Layout::left_to_right(Align::Center)), |ui| theme::check(ui, &mut on, n.as_str()).changed()).inner;
                if clicked {
                    if on {
                        pk.checked.push(n.clone());
                    } else {
                        pk.checked.retain(|x| key(x) != key(n));
                    }
                }
                if open {
                    rename_form(ui, W - 16.0, all, pk);
                }
            }
        }
    });
    if let Some(n) = toggle_pin {
        if prefs.pins.iter().any(|x| key(x) == key(&n)) {
            prefs.pins.retain(|x| key(x) != key(&n));
        } else {
            prefs.pins.push(n);
        }
        *prefs.changed = true;
    }
    ui.separator();
    let mut out = None;
    let n = pk.checked.len();
    let (row, _) = ui.allocate_exact_size(vec2(W, 30.0), Sense::hover());
    let note = if busy.is_some() {
        tr!("正在改 Jellyfin 上的影片…（可以關掉，會繼續）", "Updating videos on Jellyfin… (you can close this)").to_string()
    } else {
        trf!("已勾 {n} 個", "{n} checked")
    };
    ui.painter().text(egui::pos2(row.min.x, row.center().y), egui::Align2::LEFT_CENTER, note, theme::font(12.0), p.muted);
    let add = Btn::new(trf!("加入 {n} 個", "Add {n}")).primary().small().enabled(n > 0);
    let bw = add.width(ui);
    let br = Rect::from_min_max(egui::pos2(row.max.x - bw, row.min.y), row.max);
    if ui.scope_builder(egui::UiBuilder::new().max_rect(br), |ui| add.show(ui)).inner.clicked() {
        out = Some(std::mem::take(&mut pk.checked));
        pk.search.clear();
    }
    out
}

/// 重新命名的框（在那一列下面開啟）：打正確的寫法，下面列出相近、已經有的；按「改 N 支」開始改
fn rename_form(ui: &mut egui::Ui, w: f32, cands: &[(String, usize)], pk: &mut Picker) {
    let p = theme::pal(ui);
    let key = jellyfin::name_key;
    let Some(f) = pk.rename.as_mut() else { return };
    let (from, count) = (f.from.clone(), f.count);
    let mut go = None;
    let mut cancel = false;
    let just_opened = f.focus;
    let framed = egui::Frame::new().fill(p.accent_soft).stroke(Stroke::new(1.0, p.border)).corner_radius(8.0).inner_margin(8.0).show(ui, |ui| {
        let w = w - 16.0;
        ui.set_width(w);
        ui.label(RichText::new(trf!("把「{from}」（{count} 支）改成：", "Change “{from}” ({count} videos) to:")).font(theme::font(12.5)));
        let te = ui.add(egui::TextEdit::singleline(&mut f.to).desired_width(w).min_size(vec2(0.0, 28.0)).vertical_align(Align::Center));
        if f.focus {
            te.request_focus();
            f.focus = false;
        }
        let enter = te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        // 相近、已經有的（看起來一樣的排最前面）：按一下填進去
        let q = key(&f.to);
        let mut near: Vec<&(String, usize)> =
            cands.iter().filter(|(m, _)| *m != from && (jellyfin::spelling_diff(&from, m).is_some() || (!q.is_empty() && (key(m).contains(&q) || q.contains(&key(m)))))).collect();
        near.sort_by_key(|(m, _)| jellyfin::spelling_diff(&from, m).is_none());
        for (m, mc) in near.into_iter().take(4) {
            let (r, resp) = ui.allocate_exact_size(vec2(w, 24.0), Sense::click());
            let on = key(m) == q;
            if on || resp.hovered() {
                ui.painter().rect_filled(r, 5.0, if on { p.surface } else { p.surface2 });
            }
            ui.painter().text(egui::pos2(r.min.x + 8.0, r.center().y), egui::Align2::LEFT_CENTER, m, theme::font(12.5), p.text);
            ui.painter().text(egui::pos2(r.max.x - 8.0, r.center().y), egui::Align2::RIGHT_CENTER, trf!("{mc} 支・已經有", "{mc} · exists"), theme::font(12.0), p.muted);
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                f.to = m.clone();
            }
        }
        let to = f.to.trim().to_string();
        // 改成已經有的（只差大小寫、全形半形也算）= 合併，用那個的寫法
        let target = cands.iter().find(|(m, _)| *m != from && key(m) == key(&to));
        let same = to == from;
        let note = match target {
            _ if to.is_empty() => String::new(),
            _ if same => tr!("和原本的寫法一樣", "Same as the current spelling").to_string(),
            Some((m, tc)) => trf!(
                "合併到已經有的「{m}」：Jellyfin 上這 {count} 支影片的「{from}」會改成那個寫法，之後清單只剩一個（約 {} 支）。",
                "Merge into the existing “{m}”: “{from}” in these {count} videos on Jellyfin becomes that spelling, leaving one entry (about {}).",
                count + tc
            ),
            None => trf!("Jellyfin 上這 {count} 支影片的「{from}」會改成「{to}」。", "“{from}” in these {count} videos on Jellyfin becomes “{to}”."),
        };
        if !note.is_empty() {
            ui.add(egui::Label::new(RichText::new(note).font(theme::font(12.0)).color(p.muted)).wrap());
        }
        if let Some(d) = target.and_then(|(m, _)| jellyfin::spelling_diff(&from, m)) {
            ui.add(egui::Label::new(RichText::new(trf!("⚠ 這兩個看起來一樣，差在：{d}", "⚠ These look the same; the difference: {d}")).font(theme::font(12.0)).color(p.warn)).wrap());
        }
        let ok = !to.is_empty() && !same;
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if Btn::new(trf!("改 {count} 支", "Change {count}")).primary().small().enabled(ok).show(ui).clicked() || (enter && ok) {
                    go = Some(target.map_or(to.clone(), |(m, _)| m.clone()));
                }
                if Btn::new(tr!("取消", "Cancel")).small().ghost().show(ui).clicked() {
                    cancel = true;
                }
            });
        });
    });
    // 剛開啟：清單捲到看得到整個框（按鈕不會被擋住）
    if just_opened {
        ui.scroll_to_rect(framed.response.rect, Some(Align::Max));
    }
    if let Some(to) = go {
        pk.start = Some((from, to));
        pk.rename = None;
    } else if cancel {
        pk.rename = None;
    }
}

/// 封面預覽：選了新的就顯示新的，不然是 Jellyfin 上目前的
fn cover_box(ui: &mut egui::Ui, d: &mut JfEdit, pick: &mut bool, undo: &mut bool) {
    let p = theme::pal(ui);
    ui.vertical(|ui| {
        ui.set_width(COVER.x);
        label(ui, tr!("封面", "Cover"));
        let (rect, resp) = ui.allocate_exact_size(COVER, Sense::click());
        let tex = d.cover.as_ref().and_then(|c| c.2.as_ref()).or(d.poster.as_ref().filter(|_| !d.remove_cover));
        ui.painter().rect_filled(rect, 8.0, p.surface2);
        match tex {
            Some(t) => theme::paint_thumb_cover(ui.painter(), rect, t),
            None => theme::paint_icon(ui.painter(), Rect::from_center_size(rect.center(), vec2(28.0, 28.0)), Icon::Video, p.muted),
        }
        if resp.hovered() {
            ui.painter().rect_filled(rect, 8.0, egui::Color32::from_black_alpha(110));
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, tr!("換一張…", "Change…"), theme::font_bold(13.0), egui::Color32::WHITE);
        }
        if d.cover.is_some() {
            ui.painter().rect_stroke(rect, 8.0, Stroke::new(2.0, p.accent), egui::StrokeKind::Inside);
        }
        if resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tr!("選一張圖片當封面", "Choose an image for the cover")).clicked() {
            *pick = true;
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| match &d.cover {
            Some((name, ..)) => {
                ui.add(egui::Label::new(RichText::new(trf!("新的：{name}", "New: {name}")).font(theme::font(12.0)).color(p.accent)).truncate());
                if Btn::new(tr!("不換", "Keep old")).small().ghost().show(ui).clicked() {
                    *undo = true;
                }
            }
            None if d.remove_cover => {
                ui.label(RichText::new(tr!("存檔時移除", "Removed on save")).font(theme::font(12.0)).color(p.warn));
                if Btn::new(tr!("不移除", "Keep it")).small().ghost().show(ui).clicked() {
                    d.remove_cover = false;
                }
            }
            None => {
                if Btn::new(tr!("換封面…", "Change cover…")).small().show(ui).clicked() {
                    *pick = true;
                }
                if d.poster.is_some()
                    && Btn::new(tr!("移除", "Remove"))
                        .small()
                        .ghost()
                        .tooltip(tr!("移除 Jellyfin 上的封面（Jellyfin 之後可能自己從影片取一張）", "Remove the cover on Jellyfin (Jellyfin may pick a frame from the video later)"))
                        .show(ui)
                        .clicked()
                {
                    d.remove_cover = true;
                }
            }
        });
    });
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut d) = app.jf_edit.take() else { return };
    let mut close = false;
    let mut save = false;
    let mut pick_cover = false;
    let mut undo_cover = false;
    let mut prefs_changed = false;
    let mut retry = false;
    let year = d.year.trim().parse::<i32>().ok().filter(|y| (1900..=2999).contains(y));
    let year_ok = d.year.trim().is_empty() || year.is_some();
    // 輸入框裡還沒按 Enter 的字也算（儲存時會一起存）
    let pending = !d.new_person.trim().is_empty() || !d.new_tag.trim().is_empty();
    let changed = d.orig.as_ref().is_some_and(|o| *o != JfInfo { year, ..d.info.clone() }) || d.cover.is_some() || d.remove_cover || pending;
    let (busy_people, busy_tags) = (rename_busy(app, NameKind::Person), rename_busy(app, NameKind::Tag));
    let modal = egui::Modal::new(Id::new("jellyfin-edit")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        ui.set_width((ctx.content_rect().width() - 80.0).clamp(360.0, 600.0));
        let p = theme::pal(ui);
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr!("Jellyfin 上的影片資訊", "Details on Jellyfin")).font(theme::font_bold(17.0)));
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if Btn::icon_only(Icon::Close).ghost().small().tooltip(tr!("關閉（Esc）", "Close (Esc)")).show(ui).clicked() {
                    close = true;
                }
            });
        });
        ui.add(egui::Label::new(RichText::new(&d.name).color(p.muted)).truncate());
        ui.add_space(10.0);
        if d.orig.is_none() {
            match &d.error {
                Some(e) => {
                    ui.label(RichText::new(e).color(p.warn).font(theme::font(12.5)));
                    ui.add_space(6.0);
                    if Btn::new(tr!("重試", "Retry")).icon(Icon::Refresh).small().show(ui).clicked() {
                        retry = true;
                    }
                }
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tr!("正在讀取…", "Reading…"));
                    });
                }
            }
        } else {
            ui.add_enabled_ui(!d.saving, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                // 上面：左邊封面，右邊標題、年份
                ui.horizontal_top(|ui| {
                    cover_box(ui, &mut d, &mut pick_cover, &mut undo_cover);
                    ui.add_space(14.0);
                    ui.vertical(|ui| {
                        label(ui, tr!("標題", "Title"));
                        ui.add(egui::TextEdit::singleline(&mut d.info.title).desired_width(ui.available_width()).min_size(vec2(0.0, 30.0)).vertical_align(Align::Center));
                        if d.info.title.trim().is_empty() {
                            let msg = match title_from_people(&d.info.people) {
                                Some(t) => trf!("標題空白：儲存時用人物名稱「{t}」當標題", "Title is empty: saving uses the people's names “{t}”"),
                                None => tr!("Jellyfin 的標題不能空白：空白時保留原本的", "Jellyfin titles can't be empty; the current one is kept").to_string(),
                            };
                            ui.label(RichText::new(msg).color(p.warn).font(theme::font(12.0)));
                        }
                        ui.add_space(8.0);
                        label(ui, tr!("年份", "Year"));
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut d.year).desired_width(90.0).min_size(vec2(0.0, 30.0)).vertical_align(Align::Center).hint_text(tr!("例如 2024", "e.g. 2024")));
                            if !year_ok {
                                ui.label(RichText::new(tr!("要是 4 位數的年份", "Use a 4-digit year")).color(p.warn).font(theme::font(12.0)));
                            }
                        });
                    });
                });
                ui.add_space(10.0);
                label(ui, tr!("說明", "Description"));
                ui.add(
                    egui::TextEdit::multiline(&mut d.info.overview)
                        .desired_rows(3)
                        .desired_width(ui.available_width())
                        .hint_text(tr!("這支影片在拍什麼（電視上會顯示）", "What's in this video (shown on the TV)")),
                );
                ui.add_space(10.0);
                label(ui, tr!("人物", "People"));
                chips(
                    ui,
                    "people",
                    &mut d.info.people,
                    &mut d.new_person,
                    &d.known_people,
                    Prefs { pins: &mut app.s.jf_pin_people, recent: &app.s.jf_recent_people, changed: &mut prefs_changed, picker: &mut d.pick_people, busy: busy_people },
                    tr!("打名字，按 Enter 加入（可以一次打好幾個，用頓號分開）", "Type a name and press Enter (separate several with commas)"),
                );
                ui.add_space(10.0);
                label(ui, tr!("標籤", "Tags"));
                chips(
                    ui,
                    "tags",
                    &mut d.info.tags,
                    &mut d.new_tag,
                    &d.known_tags,
                    Prefs { pins: &mut app.s.jf_pin_tags, recent: &app.s.jf_recent_tags, changed: &mut prefs_changed, picker: &mut d.pick_tags, busy: busy_tags },
                    tr!("打標籤，按 Enter 加入", "Type a tag and press Enter"),
                );
            });
            if let Some(e) = &d.error {
                ui.add_space(8.0);
                egui::Frame::new().fill(p.warn.gamma_multiply(0.12)).corner_radius(6.0).inner_margin(8.0).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(e).color(p.warn).font(theme::font(12.5)));
                });
            }
        }
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            // 存超過 10 秒：說明伺服器還在存（一次加好幾個新標籤、人物時可能要一兩分鐘）
            let slow = d.saving && d.saving_since.is_some_and(|t| t.elapsed() > std::time::Duration::from_secs(10));
            let note = if slow {
                tr!("Jellyfin 還在儲存：一次加好幾個新標籤、人物時可能要一兩分鐘，請稍候", "Jellyfin is still saving: adding several new tags or people can take a minute or two")
            } else if d.keep_hint && changed {
                tr!("還沒儲存：要放棄這次的修改請按「取消」", "Not saved yet: click Cancel to discard your changes")
            } else {
                tr!("存到 Jellyfin，電視、手機上都看得到；伺服器上的影片檔不會動", "Saved on Jellyfin for TVs and phones; the video file on the server isn't touched")
            };
            if d.saving {
                ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
            }
            ui.label(RichText::new(note).font(theme::font(12.0)).color(if d.keep_hint && changed && !slow { p.warn } else { p.muted }));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = if d.saving { tr!("正在儲存…", "Saving…") } else { tr!("儲存", "Save") };
                let why = if !changed {
                    tr!("還沒有修改", "Nothing changed yet")
                } else if !year_ok {
                    tr!("年份要是 4 位數字", "The year must be 4 digits")
                } else {
                    ""
                };
                let r = Btn::new(label).primary().enabled(changed && year_ok && !d.saving).show(ui);
                let r = if why.is_empty() || d.saving { r } else { r.on_disabled_hover_text(why) };
                if r.clicked() {
                    save = true;
                }
                if Btn::new(tr!("取消", "Cancel")).ghost().show(ui).clicked() {
                    close = true;
                }
            });
        });
    });
    if prefs_changed {
        app.save_settings();
    }
    // 按了「改 N 支」：背景開始改（同時只改一個）
    for (kind, start) in [(NameKind::Person, d.pick_people.start.take()), (NameKind::Tag, d.pick_tags.start.take())] {
        if let Some((from, to)) = start.filter(|_| app.jf_rename.is_none()) {
            start_rename(app, d.user.clone(), kind, from, to);
        }
    }
    // Esc、點外面：沒改過就關；改過了不直接丟掉（打了一堆字按 Esc 只是想離開輸入框），提示按「取消」
    let dismiss = modal.should_close() && app.ask.is_none();
    if dismiss && changed {
        d.keep_hint = true;
    }
    if (close || dismiss && !changed) && !d.saving && app.ask.is_none() {
        return;
    }
    if retry {
        // 讀不到影片資訊：重新開一次（重新讀資訊、封面）
        let names = |v: &[(String, usize)]| v.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>();
        return open(app, d.id, d.user, d.name, names(&d.known_people), names(&d.known_tags));
    }
    if undo_cover {
        d.cover = None;
    }
    if pick_cover {
        app.jf_edit = Some(d);
        return choose_cover(app);
    }
    if save {
        // 輸入框裡還沒按 Enter 的人物、標籤也存
        commit(&mut d.info.people, &mut d.new_person);
        commit(&mut d.info.tags, &mut d.new_tag);
        // 標題空白：用人物名稱當標題（沒有人物時照舊，保留 Jellyfin 上原本的）
        if d.info.title.trim().is_empty() {
            if let Some(t) = title_from_people(&d.info.people) {
                d.info.title = t;
            }
        }
        // 這次新加的記成「最近用過」
        if let Some(o) = &d.orig {
            let new = |now: &[String], before: &[String]| -> Vec<String> { now.iter().filter(|n| !before.iter().any(|b| jellyfin::name_key(b) == jellyfin::name_key(n))).cloned().collect() };
            let (np, nt) = (new(&d.info.people, &o.people), new(&d.info.tags, &o.tags));
            if !np.is_empty() || !nt.is_empty() {
                remember(&mut app.s.jf_recent_people, &np);
                remember(&mut app.s.jf_recent_tags, &nt);
                app.save_settings();
            }
        }
        d.saving = true;
        d.saving_since = Some(std::time::Instant::now());
        d.error = None;
        let info = JfInfo { year, ..d.info.clone() };
        let (id, user, cover, remove_cover) = (d.id.clone(), d.user.clone(), d.cover.as_ref().map(|c| c.1.clone()), d.remove_cover);
        let Some(jf) = app.jellyfin() else { return };
        let (id2, title, people, tags) = (id.clone(), info.title.clone(), info.people.clone(), info.tags.clone());
        app.spawn(
            async move {
                tokio::task::spawn_blocking(move || {
                    // 送出後沒收到回應（伺服器忙、逾時、連線斷掉）時，Jellyfin 可能還在存或其實已經存好了：
                    // 每 3 秒讀回來比對，最多再等 45 秒，都改到了就算成功
                    if let Err(e) = jf.set_info(&user, &id, &info) {
                        let saved = jf.info_landed(&user, &id, &info, std::time::Duration::from_secs(45));
                        if !saved {
                            return Err(e);
                        }
                        screenrecorder_core::info!("[Jellyfin] 存影片資訊時出錯（{e}），但讀回來都改好了");
                    }
                    if let Some(c) = cover {
                        jf.set_cover(&id, &c)?;
                    } else if remove_cover {
                        jf.remove_cover(&id)?;
                    }
                    // 重新讀回來：Jellyfin 沒改到的欄位要講出來
                    let (got, file) = jf.item_info_file(&user, &id)?;
                    let lost = jellyfin::unsaved(&info, &got);
                    if !lost.is_empty() {
                        screenrecorder_core::info!("[Jellyfin] 影片資訊沒存到：{lost:?}；想存 {info:?}，讀回來 {got:?}");
                        return Err(trf!(
                            "Jellyfin 回答存好了，但重新讀取時這些沒有改到：{}。請到 Jellyfin 網頁版這支影片的「編輯中繼資料」看看這些欄位有沒有被鎖住（鎖頭圖示）",
                            "Jellyfin said it saved, but reading it back these didn't change: {}. In the Jellyfin web app, open “Edit metadata” for this video and check whether these fields are locked",
                            lost.join(tr!("、", ", "))
                        ));
                    }
                    Ok::<_, String>((got, file))
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string()))
            },
            move |app, r| match r {
                Ok((got, file)) => {
                    app.jf_edit = None;
                    // 本機有同名的那一支：也換成一樣的影片資訊
                    if let Some(file) = file {
                        app.pull_details(&file, got);
                    }
                    super::jellyfin_browse::info_saved(app, &id2, &people, &tags);
                    super::jellyfin_browse::edited(app, &id2, &title);
                    app.toast(tr!("已更新影片資訊", "Details updated"), false);
                }
                Err(e) => {
                    super::jellyfin_browse::edited(app, &id2, "");
                    if let Some(d) = app.jf_edit.as_mut() {
                        d.saving = false;
                        d.error = Some(trf!("沒有完全存到 Jellyfin：{e}", "Not everything was saved to Jellyfin: {e}"));
                    }
                }
            },
        );
    }
    app.jf_edit = Some(d);
}

/// 選好的封面：（檔名, 圖片內容, 預覽）
type Picked = (String, Vec<u8>, Option<egui::TextureHandle>);

/// 用 Windows 的開啟檔案視窗選封面（PNG、JPG 直接用；其他格式轉成 JPG）
fn choose_cover(app: &mut UiApp) {
    let ctx = app.ctx.clone();
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || -> Result<Option<Picked>, String> {
                let Some(path) = screenrecorder_core::picture::pick_file()? else { return Ok(None) };
                let bytes = std::fs::read(&path).map_err(|e| screenrecorder_core::error::plain(&e))?;
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let pm = screenrecorder_core::picture::decode_pixmap(&bytes).ok_or_else(|| tr!("看不懂這張圖片", "Couldn't read this image").to_string())?;
                let preview = Some(ctx.load_texture("jf-edit-cover", egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data()), egui::TextureOptions::LINEAR));
                let jpeg_or_png = bytes.starts_with(b"\x89PNG") || bytes.starts_with(&[0xFF, 0xD8]);
                if jpeg_or_png {
                    return Ok(Some((name, bytes, preview)));
                }
                Ok(Some((name, screenrecorder_core::shot_edit::encode_jpeg(&pm, 90)?, preview)))
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        },
        |app, r| match r {
            Ok(Some(c)) => {
                if let Some(d) = app.jf_edit.as_mut() {
                    d.cover = Some(c);
                    d.remove_cover = false;
                }
            }
            Ok(None) => {}
            Err(e) => app.toast(e, true),
        },
    );
}

/// 標題空白時用的標題：人物名稱用「、」接起來（沒有人物時 None）
fn title_from_people(people: &[String]) -> Option<String> {
    let names: Vec<&str> = people.iter().map(|n| n.trim()).filter(|n| !n.is_empty()).collect();
    (!names.is_empty()).then(|| names.join(tr!("、", ", ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_names_become_chips() {
        let mut list = vec!["媽媽".to_string()];
        let mut input = " 小明、阿嬤, 媽媽 ".to_string();
        commit(&mut list, &mut input);
        assert_eq!(list, vec!["媽媽", "小明", "阿嬤"]);
        assert!(input.is_empty());
    }

    #[test]
    fn empty_title_uses_people_names() {
        let people = vec![" 小明 ".to_string(), String::new(), "阿嬤".to_string()];
        assert_eq!(title_from_people(&people).as_deref(), Some("小明、阿嬤"));
        assert_eq!(title_from_people(&[" ".to_string()]), None);
    }
}
