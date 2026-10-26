//! 影片資訊：改 MP4 裡的標題、作者、說明等（檔案總管「內容 → 詳細資料」看到的那些）。見 core::video_tags。

use super::dialogs;
use super::jellyfin_edit::{self, Picker, Prefs};
use super::theme::{self, Btn, Icon};
use super::UiApp;
use eframe::egui::{self, Align, Id, Layout, RichText};
use screenrecorder_core::details_sync::{self, Field};
use screenrecorder_core::jellyfin::NameKind;
use screenrecorder_core::types::LibraryEntry;
use screenrecorder_core::video_tags;
use screenrecorder_core::{tr, trf};
use std::collections::HashMap;

pub struct TagsDialog {
    path: String,
    name: String,
    /// 讀出來的內容（判斷有沒有改過）；還在讀時是 None
    orig: Option<HashMap<String, String>>,
    values: HashMap<String, String>,
    saving: bool,
    error: Option<String>,
    /// 改過了又按 Esc、點外面：不關，提示要放棄請按「取消」
    keep_hint: bool,
    /// 正在和 Jellyfin 上同名的那一支同步（先不能改）
    syncing: bool,
    /// 剛換成 Jellyfin 上較新的內容
    synced_note: bool,
    /// 兩邊都改過、不一樣的欄位與 Jellyfin 上的內容（使用者選；不選就用這邊的）
    conflicts: Vec<(Field, String)>,
    /// 有設定 Jellyfin 時：類型、標籤、人物改成和 Jellyfin 影片資訊一樣的小標籤＋常用＋「全部 N 個」清單
    lists: Option<Lists>,
    /// 主畫面播放中開的：放在播放區旁邊（影片繼續播），不是蓋住整個視窗的對話框
    docked: bool,
}

impl TagsDialog {
    pub fn docked(&self) -> bool {
        self.docked
    }
}

/// 類型、標籤、人物的選擇清單（順序：類型、標籤、人物）
#[derive(Default)]
struct Lists {
    /// Jellyfin 的使用者（讀使用次數、重新命名用；還沒讀到時空的）
    user: String,
    /// Jellyfin 上用過的與用在幾支影片（讀好之前是空的）
    known: [Vec<(String, usize)>; 3],
    pick: [Picker; 3],
    /// 輸入框裡還沒按 Enter 的字
    input: [String; 3],
}

/// 用選擇清單的欄位：（欄位, 小元件的編號, 提示）
const LIST_KEYS: [&str; 3] = ["genre", "keywords", video_tags::PEOPLE_KEY];

fn list_hint(i: usize) -> &'static str {
    match i {
        0 => tr!("打類型，按 Enter 加入", "Type a genre and press Enter"),
        1 => tr!("打標籤，按 Enter 加入", "Type a tag and press Enter"),
        _ => tr!("打名字，按 Enter 加入（可以一次打好幾個，用頓號分開）", "Type a name and press Enter (separate several with commas)"),
    }
}

/// 從 Jellyfin 讀全部影片用到的類型、標籤、人物與次數
fn load_lists(app: &mut UiApp) {
    let Some(jf) = app.jellyfin() else { return };
    let preferred = app.s.jellyfin_user.clone();
    let Some(path) = app.video_tags.as_ref().map(|d| d.path.clone()) else { return };
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || {
                let user = jf.browse_user(&preferred)?;
                jf.usage(&user).map(|u| (user, u))
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        },
        move |app, r| {
            let Some(l) = app.video_tags.as_mut().filter(|d| d.path == path).and_then(|d| d.lists.as_mut()) else { return };
            match r {
                Ok((user, u)) => {
                    l.user = user;
                    // 「18+」是「只給大人看」的勾選框，不當成一般標籤列出
                    let tags = u.tags.into_iter().filter(|(t, _)| t.trim() != video_tags::ADULT_TAG).collect();
                    l.known = [u.genres, tags, u.people];
                }
                Err(e) => screenrecorder_core::info!("[影片資訊] 讀不到 Jellyfin 的類型、標籤、人物：{e}"),
            }
        },
    );
}

/// 輸入框裡還沒按 Enter 的字也加進去
fn commit_inputs(d: &mut TagsDialog) {
    let Some(l) = d.lists.as_mut() else { return };
    for (i, key) in LIST_KEYS.iter().enumerate() {
        if l.input[i].trim().is_empty() {
            continue;
        }
        let v = d.values.entry(key.to_string()).or_default();
        let mut list = video_tags::split_genres(v);
        jellyfin_edit::commit(&mut list, &mut l.input[i]);
        *v = video_tags::join_genres(&list);
    }
}

pub fn open(app: &mut UiApp, entry: LibraryEntry) {
    let path = entry.media.path.clone();
    if !video_tags::can_tag(&path) {
        return app.toast(tr!("只有 MP4 影片可以改影片資訊", "Only MP4 videos can have their details changed"), true);
    }
    let Some(ff) = app.core.ffmpeg_path() else {
        return app.toast(tr!("找不到 FFmpeg", "FFmpeg not found"), true);
    };
    app.video_tags = Some(TagsDialog {
        name: dialogs::file_name(&path),
        path: path.clone(),
        orig: None,
        values: HashMap::new(),
        saving: false,
        error: None,
        keep_hint: false,
        syncing: false,
        synced_note: false,
        conflicts: vec![],
        lists: app.jellyfin().map(|_| Lists::default()),
        docked: app.inline.is_some() && app.viewer.is_none(),
    });
    load_lists(app);
    let p = path.clone();
    app.spawn(async move { video_tags::read(&ff, &p).await }, move |app, r| {
        let Some(d) = &mut app.video_tags else { return };
        match r {
            Ok(m) => {
                d.values = m.clone();
                d.orig = Some(m);
                check_jellyfin(app, path);
            }
            Err(e) => d.error = Some(e.message().to_string()),
        }
    });
}

/// 和 Jellyfin 上同名的那一支比對：只有一邊改過就照它同步兩邊；兩邊都改了同一個欄位而且不一樣，列出來讓使用者選
fn check_jellyfin(app: &mut UiApp, path: String) {
    let (Some(jf), Some(ff)) = (app.jellyfin(), app.core.ffmpeg_path()) else { return };
    let user = app.s.jellyfin_user.clone();
    if let Some(d) = app.video_tags.as_mut() {
        d.syncing = true;
    }
    let p = path.clone();
    app.spawn(async move { details_sync::check(&jf, &user, &ff, &p).await }, move |app, r| {
        let Some(d) = app.video_tags.as_mut().filter(|d| d.path == path) else { return };
        let f = match r {
            Ok(Some(f)) => f,
            Ok(None) => return d.syncing = false,
            Err(e) => {
                d.syncing = false;
                return screenrecorder_core::info!("[影片資訊同步] 沒有和 Jellyfin 比對：{e}");
            }
        };
        if !f.plan.conflicts.is_empty() {
            // 不衝突的先換上（儲存時兩邊一起改），衝突的列出來
            d.syncing = false;
            d.values = f.plan.merged.to_local(&f.local);
            d.conflicts = f.plan.conflicts.clone();
            return;
        }
        if !f.plan.to_local && !f.plan.to_jf {
            d.syncing = false;
            details_sync::remember(&d.name, &f.plan.merged);
            return;
        }
        // 只有一邊改過：照它改另一邊（本機要改寫時，檢視器正在播這支就先放開）
        let t = if f.plan.to_local { super::viewer::release(app, &path) } else { None };
        let here = if f.plan.to_local { super::inline_play::release(app, &path) } else { None };
        let (Some(jf), Some(ff)) = (app.jellyfin(), app.core.ffmpeg_path()) else { return };
        let user = app.s.jellyfin_user.clone();
        let p = path.clone();
        app.spawn(
            async move {
                let wrote = details_sync::apply(&jf, &user, &ff, &p, &f, &f.plan.merged).await?;
                let values = video_tags::read(&ff, &p).await.map_err(|e| e.message().to_string())?;
                Ok::<_, String>((wrote, values))
            },
            move |app, r| {
                if let Some(d) = app.video_tags.as_mut().filter(|d| d.path == path) {
                    d.syncing = false;
                    match r {
                        Ok((wrote, values)) => {
                            d.synced_note = wrote;
                            d.values = values.clone();
                            d.orig = Some(values);
                        }
                        Err(e) => d.error = Some(trf!("沒有和 Jellyfin 同步好：{e}", "Couldn't sync with Jellyfin: {e}")),
                    }
                }
                app.core.cache.forget(&path);
                super::viewer::reload(app, &path, t);
                super::inline_play::resume(app, here);
            },
        );
    });
}

/// 對話框（主畫面沒在播放時）
pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    run(app, ctx, None);
}

/// 主畫面播放區旁邊的面板（main_view 給位置）
pub fn panel(app: &mut UiApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    run(app, &ctx, Some(ui));
}

/// 面板的寬度
pub const PANEL_W: f32 = 380.0;

fn run(app: &mut UiApp, ctx: &egui::Context, host: Option<&mut egui::Ui>) {
    let Some(mut d) = app.video_tags.take() else { return };
    let mut close = false;
    let mut retry = false;
    let mut save = false;
    // 以前用過的類型，加上這支影片原本的（按一下加入或移除）
    let mut known = app.s.genres.clone();
    for g in d.orig.as_ref().and_then(|o| o.get("genre")).map(|g| video_tags::split_genres(g)).unwrap_or_default() {
        if !known.iter().any(|k| k.to_lowercase() == g.to_lowercase()) {
            known.push(g);
        }
    }
    let has_jf = app.jellyfin().is_some();
    let pending = d.lists.as_ref().is_some_and(|l| l.input.iter().any(|t| !t.trim().is_empty()));
    let mut busy = [None, jellyfin_edit::rename_busy(app, NameKind::Tag), jellyfin_edit::rename_busy(app, NameKind::Person)];
    let genres_known = known.clone();
    let mut prefs_changed = false;
    let changed = pending || d.orig.as_ref().is_some_and(|o| o.iter().any(|(k, v)| d.values.get(k).is_some_and(|n| n.trim() != v.trim())));
    // part：0 = 全部（對話框），1 = 欄位（面板裡可以捲動），2 = 底下的按鈕（面板固定在最下面）
    let mut content = |ui: &mut egui::Ui, part: u8| {
        let pal = theme::pal(ui);
        if part != 2 {
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr!("影片資訊", "Video details")).font(theme::font_bold(17.0)));
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    if Btn::icon_only(Icon::Close).ghost().small().tooltip(tr!("關閉（Esc）", "Close (Esc)")).show(ui).clicked() {
                        close = true;
                    }
                });
            });
            ui.add(egui::Label::new(RichText::new(&d.name).color(pal.muted)).truncate());
            ui.add_space(8.0);
            if let Some(e) = &d.error {
                ui.label(RichText::new(e).color(pal.warn).font(theme::font(12.5)));
                ui.add_space(6.0);
                if Btn::new(tr!("重試", "Retry")).icon(Icon::Refresh).small().show(ui).clicked() {
                    retry = true;
                }
            } else if d.orig.is_none() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tr!("正在讀取…", "Reading…"));
                });
            } else {
                if d.syncing {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new(tr!("正在和 Jellyfin 上的同一支影片對照…", "Checking against the same video in Jellyfin…")).font(theme::font(12.5)).color(pal.muted));
                    });
                    ui.add_space(4.0);
                } else if d.synced_note {
                    ui.label(RichText::new(tr!("✓ Jellyfin 上改得比較新，已換成一樣的內容", "✓ Jellyfin had newer details, so they were copied here")).font(theme::font(12.5)).color(pal.ok));
                    ui.add_space(4.0);
                }
                conflicts(ui, &mut d);
                ui.add_enabled_ui(!d.saving && !d.syncing, |ui| {
                    egui::Grid::new("video-tags-form").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                        for f in video_tags::fields() {
                            ui.label(f.label);
                            let v = d.values.entry(f.key.to_string()).or_default();
                            if let (Some(i), Some(l)) = (LIST_KEYS.iter().position(|k| *k == f.key), d.lists.as_mut()) {
                                // 和 Jellyfin 影片資訊一樣：小標籤、常用、「全部 N 個」
                                let mut known = l.known[i].clone();
                                if i == 0 {
                                    for g in &genres_known {
                                        if !known.iter().any(|(n, _)| n.to_lowercase() == g.to_lowercase()) {
                                            known.push((g.clone(), 0));
                                        }
                                    }
                                }
                                let s = &mut app.s;
                                let (pins, recent) = match i {
                                    0 => (&mut s.jf_pin_genres, &s.genres),
                                    1 => (&mut s.jf_pin_tags, &s.jf_recent_tags),
                                    _ => (&mut s.jf_pin_people, &s.jf_recent_people),
                                };
                                let mut list = video_tags::split_genres(v);
                                ui.vertical(|ui| {
                                    jellyfin_edit::chips(
                                        ui,
                                        &format!("local-{}", f.key),
                                        &mut list,
                                        &mut l.input[i],
                                        &known,
                                        Prefs { pins, recent, changed: &mut prefs_changed, picker: &mut l.pick[i], busy: busy[i].take() },
                                        list_hint(i),
                                    );
                                });
                                *v = video_tags::join_genres(&list);
                                ui.end_row();
                                continue;
                            }
                            if f.key != "genre" {
                                let edit = if f.multiline { egui::TextEdit::multiline(v).desired_rows(4) } else { egui::TextEdit::singleline(v) };
                                ui.add(edit.desired_width(ui.available_width()).hint_text(f.hint));
                                ui.end_row();
                                continue;
                            }
                            ui.vertical(|ui| {
                                ui.add(egui::TextEdit::singleline(v).desired_width(ui.available_width()).hint_text(f.hint));
                                genre_chips(ui, v, &known);
                            });
                            ui.end_row();
                        }
                    });
                });
                ui.add_space(4.0);
                let adult = d.values.entry(video_tags::ADULT_KEY.to_string()).or_default();
                let mut on = adult == "1";
                ui.add_enabled_ui(!d.saving && !d.syncing, |ui| {
                    if ui
                        .checkbox(&mut on, tr!("只給大人看", "Adults only"))
                        .on_hover_text(tr!(
                            "傳到 Jellyfin 時分級設成 R、加上標籤「18+」：在小孩帳號的家長控制設最高分級（例如 PG-13）或封鎖標籤「18+」，小孩就看不到",
                            "In Jellyfin this sets the rating to R and adds the tag “18+”. Set a maximum rating (e.g. PG-13) or block the “18+” tag in a child's profile to hide it"
                        ))
                        .changed()
                    {
                        *adult = if on { "1".into() } else { String::new() };
                    }
                });
                ui.add_space(6.0);
                ui.label(
                RichText::new(tr!(
                    "寫進影片檔裡：Windows 檔案總管（按右鍵 → 內容 → 詳細資料）和多數播放器看得到。影片內容、封面、章節都不變；欄位清空就是刪掉。",
                    "Written into the video file: shown in File Explorer (right-click → Properties → Details) and most players. The video, cover and chapters stay the same; clear a field to remove it."
                ))
                .font(theme::font(12.0))
                .color(pal.muted),
            );
                if has_jf {
                    ui.label(
                    RichText::new(tr!(
                        "Jellyfin 上有同名的影片時，標題、說明、年份、標籤、人物、只給大人看兩邊會一樣（在哪邊改都會同步到另一邊）。",
                        "If Jellyfin has a video with the same file name, the title, description, year, tags, people and adults-only setting are kept the same on both sides (changes on either side sync to the other)."
                    ))
                    .font(theme::font(12.0))
                    .color(pal.muted),
                );
                }
            }
        }
        if part == 1 {
            return;
        }
        ui.add_space(if part == 0 { 10.0 } else { 6.0 });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let label = if d.saving { tr!("正在儲存…", "Saving…") } else { tr!("儲存", "Save") };
            let tip = if changed { "" } else { tr!("還沒有修改", "Nothing changed yet") };
            let can = (changed || !d.conflicts.is_empty()) && !d.saving && !d.syncing;
            if Btn::new(label).primary().enabled(can).tooltip(tip).show(ui).clicked() {
                save = true;
            }
            if Btn::new(tr!("取消", "Cancel")).ghost().show(ui).clicked() {
                close = true;
            }
            if d.keep_hint && changed {
                ui.label(RichText::new(tr!("還沒儲存：要放棄修改請按「取消」", "Not saved yet: click Cancel to discard")).font(theme::font(12.0)).color(pal.warn));
            }
        });
    };
    let dismiss = match host {
        None => {
            let modal = egui::Modal::new(Id::new("video-tags")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 80.0).clamp(320.0, 520.0));
                content(ui, 0);
            });
            modal.should_close()
        }
        Some(ui) => {
            let p = theme::pal(ui);
            let rect = ui.max_rect();
            egui::Frame::new().fill(p.surface).stroke(egui::Stroke::new(1.0, p.border)).corner_radius(8.0).inner_margin(12.0).show(ui, |ui| {
                ui.set_min_size(rect.size() - egui::vec2(24.0, 24.0));
                ui.set_max_width(rect.width() - 24.0);
                let h = (ui.available_height() - 44.0).max(80.0);
                egui::ScrollArea::vertical().id_salt("video-tags-panel").max_height(h).auto_shrink([false, false]).show(ui, |ui| content(ui, 1));
                ui.separator();
                content(ui, 2);
            });
            // 面板沒有「外面」可以點；Esc 留給主畫面播放（回到錄影畫面），面板用 ✕、取消關閉
            false
        }
    };
    // Esc、點外面：沒改過就關；改過了不直接丟掉，提示按「取消」
    if dismiss && changed {
        d.keep_hint = true;
    }
    if (close || dismiss && !changed) && !d.saving {
        return;
    }
    if retry {
        // 讀不到影片資訊：重新讀一次
        let media = screenrecorder_core::types::MediaInfo { path: d.path, ..Default::default() };
        return open(app, LibraryEntry { media, exports: vec![], hits: vec![] });
    }
    if prefs_changed {
        app.save_settings();
    }
    // 「全部 N 個」清單按了「改 N 支」：Jellyfin 上全部影片的這個標籤、人物一起改（類型不能改名）
    if let Some(l) = d.lists.as_mut() {
        let user = l.user.clone();
        let renames: Vec<(NameKind, Option<(String, String)>)> = vec![(NameKind::Tag, l.pick[1].take_rename()), (NameKind::Person, l.pick[2].take_rename())];
        l.pick[0].take_rename();
        for (kind, start) in renames {
            if let Some((from, to)) = start.filter(|_| app.jf_rename.is_none() && !user.is_empty()) {
                jellyfin_edit::start_rename(app, user.clone(), kind, from, to);
            }
        }
    }
    if save {
        commit_inputs(&mut d);
        // 這次新加的標籤、人物記成「最近用過」（和 Jellyfin 影片資訊共用）
        if d.lists.is_some() {
            let orig = d.orig.clone().unwrap_or_default();
            let added = |key: &str| -> Vec<String> {
                let before = video_tags::split_genres(orig.get(key).map(String::as_str).unwrap_or(""));
                video_tags::split_genres(d.values.get(key).map(String::as_str).unwrap_or("")).into_iter().filter(|n| !before.iter().any(|b| b.to_lowercase() == n.to_lowercase())).collect()
            };
            let (tags, people) = (added("keywords"), added(video_tags::PEOPLE_KEY));
            if !tags.is_empty() || !people.is_empty() {
                jellyfin_edit::remember(&mut app.s.jf_recent_tags, &tags);
                jellyfin_edit::remember(&mut app.s.jf_recent_people, &people);
                app.save_settings();
            }
        }
        d.saving = true;
        let path = d.path.clone();
        // 只寫改過的欄位：沒動的照原樣留著
        let orig = d.orig.clone().unwrap_or_default();
        let mut values: HashMap<String, String> = d.values.iter().filter(|(k, v)| orig.get(*k).is_none_or(|o| o.trim() != v.trim())).map(|(k, v)| (k.clone(), v.clone())).collect();
        // 標籤、人物、只給大人看一起存在 keywords 裡：改了其中一個，三個都要給
        let kw = ["keywords", video_tags::PEOPLE_KEY, video_tags::ADULT_KEY];
        if kw.iter().any(|k| values.contains_key(*k)) {
            for k in kw {
                values.insert(k.to_string(), d.values.get(k).cloned().unwrap_or_default());
            }
        }
        // 播放中的檔案換不掉：檢視器正在播這支就先放開，寫好再從同一個位置載入
        let t = super::viewer::release(app, &path);
        let here = super::inline_play::release(app, &path);
        let ff = app.core.ffmpeg_path();
        let p2 = path.clone();
        let used = video_tags::split_genres(d.values.get("genre").map(String::as_str).unwrap_or(""));
        // 沒改這邊的欄位（只是兩邊不一樣、選了用這邊的）：不用改寫檔案，只同步 Jellyfin
        app.spawn(
            async move {
                if values.is_empty() {
                    return Ok(());
                }
                details_sync::write_local(ff.as_deref(), &p2, &values).await
            },
            move |app, r| {
                match r {
                    Ok(()) => {
                        app.video_tags = None;
                        // 搜尋也找影片資訊：快取裡舊的那一份丟掉（修改時間沒變，大小可能也一樣）
                        app.core.cache.forget(&path);
                        remember_genres(app, &used);
                        app.toast(trf!("已更新影片資訊：{}", "Video details updated: {}", dialogs::file_name(&path)), false);
                        app.push_details(&path);
                    }
                    Err(e) => {
                        if let Some(d) = &mut app.video_tags {
                            d.saving = false;
                        }
                        app.toast(e.message().to_string(), true);
                    }
                }
                super::viewer::reload(app, &path, t);
                super::inline_play::resume(app, here);
            },
        );
    }
    app.video_tags = Some(d);
}

/// 兩邊都改過、不一樣的欄位：顯示 Jellyfin 上的內容，按「用 Jellyfin 的」換過來；不選就照這邊的（儲存時 Jellyfin 也改成這邊的）
fn conflicts(ui: &mut egui::Ui, d: &mut TagsDialog) {
    if d.conflicts.is_empty() {
        return;
    }
    let pal = theme::pal(ui);
    let mut take = None;
    egui::Frame::new().fill(pal.surface2).corner_radius(6.0).inner_margin(8.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(tr!("Jellyfin 上也改過，這幾個和這裡不一樣：", "These were also changed in Jellyfin and differ from here:")).font(theme::font_bold(12.5)).color(pal.warn));
        for (i, (f, v)) in d.conflicts.iter().enumerate() {
            ui.horizontal(|ui| {
                let shown = if v.trim().is_empty() { tr!("（空的）", "(empty)").to_string() } else { v.replace('\n', " ") };
                ui.add(egui::Label::new(RichText::new(trf!("{}：「{}」", "{}: “{}”", f.label(), shown)).font(theme::font(12.5))).truncate());
                if Btn::new(tr!("用 Jellyfin 的", "Use Jellyfin's")).small().show(ui).clicked() {
                    take = Some(i);
                }
            });
        }
        ui.label(RichText::new(tr!("不選就用這裡的；儲存後兩邊都會一樣", "Otherwise this side is kept; after saving both sides match")).font(theme::font(12.0)).color(pal.muted));
    });
    ui.add_space(6.0);
    if let Some(i) = take {
        let (f, v) = d.conflicts.remove(i);
        d.values.insert(f.local_key().to_string(), v);
    }
}

/// 類型欄下面的小標籤：以前用過的類型，按一下加入或移除（已選的反白）
fn genre_chips(ui: &mut egui::Ui, value: &mut String, known: &[String]) {
    if known.is_empty() {
        return;
    }
    let mut chosen = video_tags::split_genres(value);
    let mut toggled = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        for g in known {
            let on = chosen.iter().any(|c| c.to_lowercase() == g.to_lowercase());
            let tip = if on { tr!("按一下移除", "Click to remove") } else { tr!("按一下加入", "Click to add") };
            if theme::chip(ui, g, if on { theme::Tone::Accent } else { theme::Tone::Plain }, true).on_hover_text(tip).clicked() {
                toggled = Some((g.clone(), on));
            }
        }
    });
    if let Some((g, on)) = toggled {
        if on {
            chosen.retain(|c| c.to_lowercase() != g.to_lowercase());
        } else {
            chosen.push(g);
        }
        *value = video_tags::join_genres(&chosen);
    }
}

/// 存好之後記住用過的類型（最近用的在前）
fn remember_genres(app: &mut UiApp, used: &[String]) {
    if used.is_empty() {
        return;
    }
    let mut list = used.to_vec();
    list.extend(app.s.genres.iter().filter(|g| !used.iter().any(|u| u.to_lowercase() == g.to_lowercase())).cloned());
    list.truncate(super::settings::RECENT_GENRES);
    if list != app.s.genres {
        app.s.genres = list;
        app.save_settings();
    }
}
