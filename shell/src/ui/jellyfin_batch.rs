//! Jellyfin 分頁勾選好幾支 →「整理人物、標籤…」：一次加、移除人物和標籤，改年份。
//! 上面列出勾選的影片現在有的（全部都有的實心，只有幾支有的寫幾支），可以從全部移除或補給全部；
//! 下面和影片資訊一樣打字或點常用的加入。按「改 N 支」後在背景一支一支改（只動這次改的），
//! 分頁下面顯示進度，關掉檔案庫也會繼續。見 core::jellyfin 的 BatchEdit、edit_many

use super::jellyfin_edit::{self, Picker, Prefs};
use super::theme::{self, Btn, Icon};
use super::UiApp;
use eframe::egui::{self, pos2, vec2, Align, Id, Layout, Rect, RichText, Sense, Stroke};
use screenrecorder_core::jellyfin::{self, BatchEdit, JfItem};
use screenrecorder_core::{tr, trf};
use std::sync::{Arc, Mutex};

/// 勾選的影片現在有的一個人物或標籤要怎麼改
#[derive(Clone, Copy, PartialEq)]
enum Act {
    Keep,
    /// 從全部移除
    Remove,
    /// 補給還沒有的
    AddAll,
}

/// 勾選的影片現在有的一個人物或標籤：名稱、幾支有、要怎麼改
struct Have {
    name: String,
    count: usize,
    act: Act,
}

pub struct JfBatch {
    user: String,
    ids: Vec<String>,
    people: Vec<Have>,
    tags: Vec<Have>,
    /// 要加給全部的（新打的、點常用的）
    add_people: Vec<String>,
    add_tags: Vec<String>,
    new_person: String,
    new_tag: String,
    known_people: Vec<(String, usize)>,
    known_tags: Vec<(String, usize)>,
    pick_people: Picker,
    pick_tags: Picker,
    /// 年份：0 不改、1 都改成、2 清掉
    year_mode: u8,
    year: String,
    /// 現在的年份（沒有年份的是 None）與幾支，多的在前
    years_now: Vec<(Option<i32>, usize)>,
}

/// 背景正在整理的（Jellyfin 分頁下面畫進度）
pub struct BatchRun {
    pub progress: Arc<Mutex<(usize, usize)>>,
    /// 按了「取消」：改完正在改的那一支就停
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
}

/// 名稱依只差空白、大小寫、全形半形的合在一起，算每個有幾支；多的在前、再依名稱
fn haves(lists: impl Iterator<Item = Vec<String>>) -> Vec<Have> {
    let mut out: Vec<Have> = vec![];
    for list in lists {
        let mut seen = std::collections::HashSet::new();
        for n in list.into_iter().map(|n| n.trim().to_string()).filter(|n| !n.is_empty()) {
            let key = jellyfin::name_key(&n);
            if !seen.insert(key.clone()) {
                continue;
            }
            match out.iter_mut().find(|h| jellyfin::name_key(&h.name) == key) {
                Some(h) => h.count += 1,
                None => out.push(Have { name: n, count: 1, act: Act::Keep }),
            }
        }
    }
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    out
}

/// 開啟（items 是勾選的影片；known 是伺服器上的人物、標籤與用在幾支）
pub fn open(app: &mut UiApp, user: String, items: Vec<JfItem>, known_people: Vec<(String, usize)>, known_tags: Vec<(String, usize)>) {
    let mut years: Vec<(Option<i32>, usize)> = vec![];
    for it in &items {
        match years.iter_mut().find(|(y, _)| *y == it.year) {
            Some((_, c)) => *c += 1,
            None => years.push((it.year, 1)),
        }
    }
    years.sort_by_key(|y| std::cmp::Reverse(y.1));
    app.jf_batch = Some(JfBatch {
        user,
        ids: items.iter().map(|i| i.id.clone()).collect(),
        people: haves(items.iter().map(|i| i.people.clone())),
        tags: haves(items.iter().map(|i| i.tags.clone())),
        add_people: vec![],
        add_tags: vec![],
        new_person: String::new(),
        new_tag: String::new(),
        known_people,
        known_tags,
        pick_people: Picker::default(),
        pick_tags: Picker::default(),
        year_mode: 0,
        year: String::new(),
        years_now: years,
    });
}

/// 現在有的一顆：全部都有的實心藍、只有幾支有的白底虛線框寫「2/5」；
/// 右邊是動作（移除 ×、補給全部 ＋；改了的是復原 ↺），從全部移除的劃線變紅、補給全部的變綠
fn have_chip(ui: &mut egui::Ui, h: &mut Have, total: usize) {
    let p = theme::pal(ui);
    let partial = h.count < total;
    let count = if partial { format!(" {}/{total}", h.count) } else { String::new() };
    let note = match h.act {
        Act::Keep => String::new(),
        Act::Remove => tr!("（要移除）", " (remove)").to_string(),
        Act::AddAll => tr!("（補給全部）", " (add to all)").to_string(),
    };
    let fg = match h.act {
        Act::Keep if partial => p.text,
        Act::Keep => p.accent,
        Act::Remove => p.rec,
        Act::AddAll => p.ok,
    };
    let name = ui.painter().layout_no_wrap(format!("{}{note}", h.name), theme::font(12.5), fg);
    let num = ui.painter().layout_no_wrap(count, theme::font(12.0), p.muted);
    // 動作：沒改的 ×（只有幾支有的前面多一個 ＋），改了的 ↺
    let icons: Vec<Icon> = match h.act {
        Act::Keep if partial => vec![Icon::Plus, Icon::Close],
        Act::Keep => vec![Icon::Close],
        _ => vec![Icon::Undo],
    };
    let w = 10.0 + name.size().x + num.size().x + 6.0 + icons.len() as f32 * 20.0 + 4.0;
    let (rect, _) = ui.allocate_exact_size(vec2(w, 24.0), Sense::hover());
    let (fill, stroke) = match h.act {
        Act::Keep if partial => (p.surface, Stroke::new(1.0, p.accent.gamma_multiply(0.5))),
        Act::Keep => (p.accent_soft, Stroke::NONE),
        Act::Remove => (p.rec_soft, Stroke::NONE),
        Act::AddAll => (p.ok_soft, Stroke::NONE),
    };
    ui.painter().rect(rect, 12.0, fill, stroke, egui::StrokeKind::Inside);
    let y = rect.center().y;
    let name_w = name.size().x;
    let name_pos = pos2(rect.min.x + 10.0, y - name.size().y / 2.0);
    ui.painter().galley(name_pos, name, fg);
    if h.act == Act::Remove {
        ui.painter().hline(name_pos.x..=name_pos.x + name_w, y, Stroke::new(1.0, p.rec));
    }
    ui.painter().galley(pos2(rect.min.x + 10.0 + name_w, y - num.size().y / 2.0), num, p.muted);
    let tip_all = trf!("{}：勾選的 {total} 支都有", "{}: all {total} selected videos have it", h.name);
    let tip_some = trf!("{}：勾選的 {total} 支裡有 {} 支", "{}: {} of the {total} selected videos have it", h.name, h.count);
    ui.interact(rect, ui.id().with(("have", &h.name)), Sense::hover()).on_hover_text(if partial { tip_some } else { tip_all });
    for (i, icon) in icons.iter().enumerate() {
        let c = pos2(rect.max.x - 14.0 - (icons.len() - 1 - i) as f32 * 20.0, y);
        let hit = Rect::from_center_size(c, vec2(20.0, 22.0));
        let r = ui.interact(hit, ui.id().with(("have-act", &h.name, i)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
        let r = match icon {
            Icon::Plus => r.on_hover_text(tr!("補給勾選的全部影片", "Add it to all selected videos")),
            Icon::Close => r.on_hover_text(tr!("從勾選的全部影片移除", "Remove it from all selected videos")),
            _ => r.on_hover_text(tr!("復原（不改）", "Undo (leave as is)")),
        };
        if r.hovered() {
            ui.painter().circle_filled(c, 9.0, fg.gamma_multiply(0.15));
        }
        theme::paint_icon(ui.painter(), Rect::from_center_size(c, vec2(10.0, 10.0)), *icon, fg);
        if r.clicked() {
            h.act = match icon {
                Icon::Plus => Act::AddAll,
                Icon::Close => Act::Remove,
                _ => Act::Keep,
            };
        }
    }
}

/// 一類（人物或標籤）：現在有的一排，下面是加給全部的輸入框與常用
fn section(ui: &mut egui::Ui, haves: &mut [Have], total: usize, empty: &str) {
    if haves.is_empty() {
        ui.label(RichText::new(empty).font(theme::font(12.0)).color(theme::pal(ui).muted));
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        for h in haves.iter_mut() {
            have_chip(ui, h, total);
        }
    });
}

/// 要改的（年份不是 4 位數時 None）
fn edit_of(d: &JfBatch) -> Option<BatchEdit> {
    let pick = |haves: &[Have], act: Act| haves.iter().filter(|h| h.act == act).map(|h| h.name.clone()).collect::<Vec<_>>();
    let mut add_people = pick(&d.people, Act::AddAll);
    add_people.extend(d.add_people.iter().cloned());
    let mut add_tags = pick(&d.tags, Act::AddAll);
    add_tags.extend(d.add_tags.iter().cloned());
    // 輸入框裡還沒按 Enter 的也算
    let mut np = vec![];
    jellyfin_edit::commit(&mut np, &mut d.new_person.clone());
    add_people.extend(np);
    let mut nt = vec![];
    jellyfin_edit::commit(&mut nt, &mut d.new_tag.clone());
    add_tags.extend(nt);
    let year = match d.year_mode {
        1 => Some(Some(d.year.trim().parse::<i32>().ok().filter(|y| (1900..=2999).contains(y))?)),
        2 => Some(None),
        _ => None,
    };
    Some(BatchEdit { add_people, remove_people: pick(&d.people, Act::Remove), add_tags, remove_tags: pick(&d.tags, Act::Remove), year })
}

/// 要改什麼的一句話（例如「加 1 個人物、移除 1 個標籤、年份改成 2024」）
fn summary(e: &BatchEdit) -> String {
    let mut parts = vec![];
    let n = |v: &Vec<String>| v.len();
    if n(&e.add_people) > 0 {
        parts.push(trf!("加 {} 個人物", "add {} people", n(&e.add_people)));
    }
    if n(&e.remove_people) > 0 {
        parts.push(trf!("移除 {} 個人物", "remove {} people", n(&e.remove_people)));
    }
    if n(&e.add_tags) > 0 {
        parts.push(trf!("加 {} 個標籤", "add {} tags", n(&e.add_tags)));
    }
    if n(&e.remove_tags) > 0 {
        parts.push(trf!("移除 {} 個標籤", "remove {} tags", n(&e.remove_tags)));
    }
    match e.year {
        Some(Some(y)) => parts.push(trf!("年份改成 {y}", "set the year to {y}")),
        Some(None) => parts.push(tr!("清掉年份", "clear the year").to_string()),
        None => {}
    }
    parts.join(tr!("、", ", "))
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut d) = app.jf_batch.take() else { return };
    let total = d.ids.len();
    let mut close = false;
    let mut go = None;
    let mut prefs_changed = false;
    // 常用裡不列勾選的影片都已經有的（移除的還是列，可以再加回去）
    let on_all = |haves: &[Have]| haves.iter().filter(|h| h.count >= total && h.act == Act::Keep).map(|h| jellyfin::name_key(&h.name)).collect::<Vec<_>>();
    let (all_p, all_t) = (on_all(&d.people), on_all(&d.tags));
    let known_people: Vec<(String, usize)> = d.known_people.iter().filter(|(n, _)| !all_p.contains(&jellyfin::name_key(n))).cloned().collect();
    let known_tags: Vec<(String, usize)> = d.known_tags.iter().filter(|(n, _)| !all_t.contains(&jellyfin::name_key(n))).cloned().collect();
    let modal = egui::Modal::new(Id::new("jellyfin-batch")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        ui.set_width((ctx.content_rect().width() - 80.0).clamp(380.0, 640.0));
        let p = theme::pal(ui);
        ui.horizontal(|ui| {
            ui.label(RichText::new(trf!("整理 {total} 支影片", "Organize {total} videos")).font(theme::font_bold(17.0)));
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if Btn::icon_only(Icon::Close).ghost().small().tooltip(tr!("關閉（Esc）", "Close (Esc)")).show(ui).clicked() {
                    close = true;
                }
            });
        });
        ui.label(
            RichText::new(tr!(
                "實心的是勾選的影片都有；白底的只有幾支有（寫幾支）。按 × 從全部移除，按 ＋ 補給全部；下面打字或點常用的加給全部。",
                "Solid ones are on every selected video; outlined ones only on some (with the count). Click × to remove from all, ＋ to add to all; type below or click a suggestion to add to all."
            ))
            .font(theme::font(12.0))
            .color(p.muted),
        );
        ui.add_space(10.0);
        ui.spacing_mut().item_spacing.y = 6.0;
        jellyfin_edit::label(ui, tr!("人物", "People"));
        section(ui, &mut d.people, total, tr!("勾選的影片都還沒有人物", "None of the selected videos have people yet"));
        jellyfin_edit::chips(
            ui,
            "batch-people",
            &mut d.add_people,
            &mut d.new_person,
            &known_people,
            Prefs { pins: &mut app.s.jf_pin_people, recent: &app.s.jf_recent_people, changed: &mut prefs_changed, picker: &mut d.pick_people, busy: None },
            tr!("加給全部：打名字按 Enter（可以用頓號分開好幾個）", "Add to all: type a name and press Enter (separate several with commas)"),
        );
        ui.separator();
        jellyfin_edit::label(ui, tr!("標籤", "Tags"));
        section(ui, &mut d.tags, total, tr!("勾選的影片都還沒有標籤", "None of the selected videos have tags yet"));
        jellyfin_edit::chips(
            ui,
            "batch-tags",
            &mut d.add_tags,
            &mut d.new_tag,
            &known_tags,
            Prefs { pins: &mut app.s.jf_pin_tags, recent: &app.s.jf_recent_tags, changed: &mut prefs_changed, picker: &mut d.pick_tags, busy: None },
            tr!("加給全部：打標籤按 Enter", "Add to all: type a tag and press Enter"),
        );
        ui.separator();
        ui.horizontal(|ui| {
            jellyfin_edit::label(ui, tr!("年份", "Year"));
            ui.add_space(6.0);
            let modes = [tr!("不改", "Leave as is"), tr!("都改成", "Set all to"), tr!("清掉", "Clear")];
            egui::ComboBox::from_id_salt("jf-batch-year").selected_text(modes[d.year_mode as usize]).width(110.0).show_ui(ui, |ui| {
                for (i, m) in modes.iter().enumerate() {
                    ui.selectable_value(&mut d.year_mode, i as u8, *m);
                }
            });
            if d.year_mode == 1 {
                ui.add(egui::TextEdit::singleline(&mut d.year).desired_width(80.0).hint_text(tr!("例如 2024", "e.g. 2024")));
            }
            let now: Vec<String> = d
                .years_now
                .iter()
                .map(|(y, c)| match y {
                    Some(y) => trf!("{y}（{c} 支）", "{y} ({c})"),
                    None => trf!("沒有（{c} 支）", "none ({c})"),
                })
                .collect();
            ui.label(RichText::new(trf!("現在：{}", "Now: {}", now.join(tr!("、", ", ")))).font(theme::font(12.0)).color(p.muted));
        });
        let edit = edit_of(&d);
        if d.year_mode == 1 && edit.is_none() {
            ui.label(RichText::new(tr!("年份要是 4 位數", "Use a 4-digit year")).font(theme::font(12.0)).color(p.warn));
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let what = edit.as_ref().filter(|e| !e.is_empty()).map(summary).unwrap_or_else(|| tr!("還沒有要改的", "Nothing to change yet").to_string());
            ui.label(RichText::new(trf!("要改：{what}", "Changes: {what}")).font(theme::font(12.0)).color(p.muted));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let ok = edit.as_ref().is_some_and(|e| !e.is_empty()) && app.jf_batch_run.is_none();
                let tip = if app.jf_batch_run.is_some() { tr!("上一次的整理還沒改完", "The previous batch is still running") } else { "" };
                if Btn::new(trf!("改 {total} 支", "Change {total}")).primary().enabled(ok).tooltip(tip).show(ui).clicked() {
                    go = edit.clone();
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
    if let Some(e) = go {
        jellyfin_edit::remember(&mut app.s.jf_recent_people, &e.add_people);
        jellyfin_edit::remember(&mut app.s.jf_recent_tags, &e.add_tags);
        app.save_settings();
        return start(app, d.user, d.ids, e);
    }
    if (close || modal.should_close()) && app.ask.is_none() {
        return;
    }
    app.jf_batch = Some(d);
}

/// 背景一支一支改；改好通知、清單上的人物標籤跟著換、「依這些找」重讀
fn start(app: &mut UiApp, user: String, ids: Vec<String>, edit: BatchEdit) {
    let Some(jf) = app.jellyfin() else { return };
    let progress = Arc::new(Mutex::new((0usize, ids.len())));
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    app.jf_batch_run = Some(BatchRun { progress: progress.clone(), cancel: cancel.clone() });
    let ctx = app.ctx.clone();
    let (ids2, e2) = (ids.clone(), edit.clone());
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || {
                let stop = cancel.clone();
                let r = jf.edit_many(&user, &ids2, &e2, &|done, total| {
                    *progress.lock().unwrap_or_else(|e| e.into_inner()) = (done, total);
                    ctx.request_repaint();
                    !stop.load(std::sync::atomic::Ordering::Relaxed)
                });
                (r, cancel.load(std::sync::atomic::Ordering::Relaxed))
            })
            .await
            .unwrap_or_else(|e| (Err(e.to_string()), false))
        },
        move |app, (r, cancelled)| {
            app.jf_batch_run = None;
            match r {
                Ok(n) if cancelled => app.toast(trf!("已停止：改好 {n} 支（已經改的照樣保留）", "Stopped: {n} videos changed (those stay changed)"), false),
                Ok(n) => app.toast(trf!("✓ {n} 支影片整理好了", "✓ Organized {n} videos"), false),
                Err(e) => app.toast(trf!("沒有全部整理好：{e}", "Not every video was changed: {e}"), true),
            }
            super::jellyfin_browse::batch_done(app, &ids, &edit);
        },
    );
}
