//! 全部截圖的「用點的找圖」：左邊的瀏覽欄（程式、內容類型、常見詞、時間、顏色…）、月曆、搜尋框的建議。
//! 使用者不用記得關鍵字：看到選項就認得，點幾下縮小範圍。

use super::{shot_dirs, LibraryDialog};
use crate::ui::dialogs::day_label;
use crate::ui::theme::{self, Btn, Icon};
use crate::ui::UiApp;
use eframe::egui::{self, pos2, vec2, Align2, Color32, CornerRadius, Id, Rect, RichText, Sense, Stroke, UiBuilder};
use screenrecorder_core::actions;
use screenrecorder_core::facets::CalDay;
use screenrecorder_core::types::{FacetOption, FacetQuery};
use screenrecorder_core::{tr, trf};
use std::time::Instant;

/// 瀏覽欄的寬度
pub const PANEL_W: f32 = 212.0;

/// 月曆：看哪一個月、每一天的張數與常見詞（None = 讀取中）
pub struct CalView {
    pub year: i32,
    pub month: u32,
    pub days: Option<Vec<CalDay>>,
    pub seq: u64,
}

impl CalView {
    pub fn this_month() -> Self {
        use chrono::Datelike;
        let t = chrono::Local::now().date_naive();
        Self { year: t.year(), month: t.month(), days: None, seq: 0 }
    }
}

/// 搜尋框的建議：圖裡真的有的詞與張數
#[derive(Default)]
pub struct Suggest {
    /// 建議是為了這段字
    pub query: String,
    pub list: Vec<(String, usize)>,
    /// 打字後等一下再找
    pub at: Option<Instant>,
    /// 顯示中（按 Esc、選了一個、搜尋框沒有焦點時關掉）
    pub open: bool,
    /// 滑鼠在建議清單上（點的時候搜尋框會先失去焦點，不能因此關掉）
    pub hover: bool,
}

// ───────────── 瀏覽欄 ─────────────

fn toggle(list: &mut Vec<String>, id: &str) {
    if let Some(i) = list.iter().position(|x| x == id) {
        list.remove(i);
    } else {
        list.push(id.to_string());
    }
}

/// 一個選項：左邊勾選框、名稱、右邊張數；回傳有沒有點
fn option_row(ui: &mut egui::Ui, o: &FacetOption, on: bool, dot: Option<Color32>) -> bool {
    let p = theme::pal(ui);
    let w = ui.available_width();
    let (r, resp) = ui.allocate_exact_size(vec2(w, 24.0), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if on {
        ui.painter().rect_filled(r, CornerRadius::same(5), p.accent_soft);
    } else if resp.hovered() {
        ui.painter().rect_filled(r, CornerRadius::same(5), p.surface2);
    }
    // 勾選框
    let b = Rect::from_center_size(pos2(r.min.x + 12.0, r.center().y), vec2(13.0, 13.0));
    if on {
        ui.painter().rect_filled(b, CornerRadius::same(3), p.accent);
        theme::paint_icon(ui.painter(), b.shrink(1.5), Icon::Check, p.accent_ink);
    } else {
        ui.painter().rect_stroke(b, CornerRadius::same(3), Stroke::new(1.2, p.border_strong), egui::StrokeKind::Inside);
    }
    let mut x = r.min.x + 25.0;
    if let Some(c) = dot {
        ui.painter().circle_filled(pos2(x + 5.0, r.center().y), 5.0, c);
        ui.painter().circle_stroke(pos2(x + 5.0, r.center().y), 5.0, Stroke::new(1.0, p.border_strong));
        x += 15.0;
    }
    let count = o.count.to_string();
    let cg = ui.painter().layout_no_wrap(count, theme::font(11.5), p.muted);
    let max_w = r.max.x - x - cg.size().x - 12.0;
    let lg = ui.painter().layout(o.label.clone(), theme::font(12.5), p.text, f32::INFINITY);
    let lg = if lg.size().x > max_w {
        let mut job = egui::text::LayoutJob::simple_singleline(o.label.clone(), theme::font(12.5), p.text);
        job.wrap = egui::text::TextWrapping::truncate_at_width(max_w);
        ui.painter().layout_job(job)
    } else {
        lg
    };
    ui.painter().galley(pos2(x, r.center().y - lg.size().y / 2.0), lg, p.text);
    ui.painter().galley(pos2(r.max.x - 6.0 - cg.size().x, r.center().y - cg.size().y / 2.0), cg, p.muted);
    resp.clicked()
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(8.0);
    ui.label(RichText::new(title).font(theme::font_bold(12.5)).color(theme::pal(ui).muted));
    ui.add_space(2.0);
}

/// 常見詞：一顆一顆的標籤（換行排）
fn term_chips(ui: &mut egui::Ui, opts: &[FacetOption], q: &mut FacetQuery) -> bool {
    let p = theme::pal(ui);
    let mut changed = false;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
        for o in opts {
            let on = q.terms.contains(&o.id);
            let text = format!("{} {}", o.label, o.count);
            let g = ui.painter().layout_no_wrap(text, theme::font(12.0), if on { p.accent_ink } else { p.text });
            let (r, resp) = ui.allocate_exact_size(g.size() + vec2(14.0, 8.0), Sense::click());
            let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
            let fill = if on {
                p.accent
            } else if resp.hovered() {
                p.surface2
            } else {
                p.surface
            };
            ui.painter().rect_filled(r, CornerRadius::same(11), fill);
            if !on {
                ui.painter().rect_stroke(r, CornerRadius::same(11), Stroke::new(1.0, p.border), egui::StrokeKind::Inside);
            }
            ui.painter().galley(r.min + vec2(7.0, 4.0), g, p.text);
            if resp.clicked() {
                toggle(&mut q.terms, &o.id);
                changed = true;
            }
        }
    });
    changed
}

fn color_of(id: &str) -> Color32 {
    match id {
        "dark" => Color32::from_rgb(0x22, 0x25, 0x2b),
        "light" => Color32::from_rgb(0xf6, 0xf6, 0xf4),
        "red" => Color32::from_rgb(0xe0, 0x3e, 0x3e),
        "yellow" => Color32::from_rgb(0xf2, 0xa9, 0x1c),
        "green" => Color32::from_rgb(0x2f, 0xa8, 0x5a),
        "blue" => Color32::from_rgb(0x2f, 0x7c, 0xe8),
        _ => Color32::from_rgb(0x8e, 0x5c, 0xd9),
    }
}

/// 一組選項（程式、內容類型…）：每一個一列
fn rows(ui: &mut egui::Ui, opts: &[FacetOption], sel: &mut Vec<String>, dots: bool) -> bool {
    let mut changed = false;
    for o in opts {
        let on = sel.contains(&o.id);
        if option_row(ui, o, on, dots.then(|| color_of(&o.id))) {
            toggle(sel, &o.id);
            changed = true;
        }
    }
    changed
}

/// 左邊的瀏覽欄。回傳條件有沒有改變
pub fn panel(ui: &mut egui::Ui, d: &mut LibraryDialog, rect: Rect) {
    let p = theme::pal(ui);
    ui.painter().rect_filled(rect, CornerRadius::same(theme::RADIUS_SM), p.surface2.gamma_multiply(0.5));
    let inner = rect.shrink2(vec2(8.0, 6.0));
    let ui = &mut ui.new_child(UiBuilder::new().max_rect(inner).id_salt("browsePanel"));
    let mut changed = false;
    // 標題列：瀏覽、清除條件
    ui.horizontal(|ui| {
        ui.label(RichText::new(tr!("用點的找", "Browse")).font(theme::font_bold(14.0)));
        let n = d.facet.count();
        if n > 0 {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if Btn::new(trf!("清除（{n}）", "Clear ({n})")).ghost().small().tooltip(tr!("清除全部條件", "Clear all filters")).show(ui).clicked() {
                    d.facet = FacetQuery::default();
                    changed = true;
                }
            });
        }
    });
    egui::ScrollArea::vertical().id_salt("browseScroll").auto_shrink([false, false]).show(ui, |ui| {
        ui.set_width(ui.available_width() - 4.0);
        ui.spacing_mut().item_spacing.y = 1.0;
        // 月曆點的那一天
        if let Some(day) = d.facet.day.clone() {
            ui.add_space(4.0);
            let label = chrono::NaiveDate::parse_from_str(&day, "%Y-%m-%d").map(|x| day_label(x, chrono::Local::now().date_naive())).unwrap_or(day);
            if option_row(ui, &FacetOption { id: String::new(), label, count: d.data.as_ref().map(|x| x.total).unwrap_or(0) }, true, None) {
                d.facet.day = None;
                changed = true;
            }
        }
        let Some(fc) = d.data.as_ref().and_then(|x| x.facets.clone()) else {
            ui.add_space(8.0);
            ui.label(theme::muted(ui, tr!("整理中…", "Sorting…")));
            return;
        };
        if fc.pending_text > 0 {
            ui.add_space(4.0);
            ui.label(RichText::new(trf!("還有 {} 張的字沒讀完，分類會越來越完整", "{} screenshots still being read; categories will fill in", fc.pending_text)).font(theme::font(11.5)).color(p.muted));
        }
        // 子資料夾（有兩個以上的資料夾才列）
        if !fc.folders.is_empty() {
            section(ui, tr!("資料夾", "Folder"));
            changed |= rows(ui, &fc.folders, &mut d.facet.folders, false);
        }
        section(ui, tr!("程式", "App"));
        if fc.apps.is_empty() {
            ui.label(RichText::new(tr!("之後的截圖會記下截圖時在用的程式", "New screenshots will remember the app you were using")).font(theme::font(11.5)).color(p.muted));
        } else {
            changed |= rows(ui, &fc.apps, &mut d.facet.apps, false);
        }
        if !fc.when.is_empty() || !fc.hours.is_empty() {
            section(ui, tr!("時間", "When"));
            changed |= rows(ui, &fc.when, &mut d.facet.when, false);
            if !fc.hours.is_empty() {
                ui.add_space(4.0);
                changed |= rows(ui, &fc.hours, &mut d.facet.hours, false);
            }
        }
        if !fc.other.is_empty() {
            section(ui, tr!("其他", "Other"));
            changed |= rows(ui, &fc.other, &mut d.facet.other, false);
        }
        // 內容、常見詞、顏色比較少用：收在「更多分類」，有選的時候自動打開
        if !fc.kinds.is_empty() || !fc.terms.is_empty() || !fc.colors.is_empty() {
            let picked = !d.facet.kinds.is_empty() || !d.facet.terms.is_empty() || !d.facet.colors.is_empty();
            let id = egui::Id::new("browse-more");
            let mut open = picked || ui.data(|x| x.get_temp::<bool>(id)).unwrap_or(false);
            ui.add_space(8.0);
            let (r, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 22.0), egui::Sense::click());
            let icon = egui::Rect::from_center_size(egui::pos2(r.left() + 7.0, r.center().y), egui::vec2(12.0, 12.0));
            theme::paint_icon(ui.painter(), icon, if open { Icon::ChevD } else { Icon::ChevR }, p.muted);
            ui.painter().text(egui::pos2(r.left() + 18.0, r.center().y), Align2::LEFT_CENTER, tr!("更多分類", "More categories"), theme::font_bold(12.5), p.muted);
            if resp.on_hover_text(tr!("依內容類型、圖裡常出現的詞、顏色找", "Browse by content type, common words and color")).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && !picked
            {
                open = !open;
                ui.data_mut(|x| x.insert_temp(id, open));
            }
            if open {
                if !fc.kinds.is_empty() {
                    section(ui, tr!("內容", "Content"));
                    changed |= rows(ui, &fc.kinds, &mut d.facet.kinds, false);
                }
                if !fc.terms.is_empty() {
                    section(ui, tr!("圖裡常出現的詞", "Common words"));
                    changed |= term_chips(ui, &fc.terms, &mut d.facet);
                }
                if !fc.colors.is_empty() {
                    section(ui, tr!("顏色", "Colour"));
                    changed |= rows(ui, &fc.colors, &mut d.facet.colors, true);
                }
            }
        }
        ui.add_space(8.0);
    });
    if changed {
        d.page = 1;
        d.dirty = true;
    }
}

// ───────────── 月曆 ─────────────

/// 讀取月曆的資料（換月份時）
fn load_calendar(app: &mut UiApp, d: &mut LibraryDialog) {
    let Some(c) = &mut d.calendar else { return };
    let want = (c.year as u64) * 100 + c.month as u64;
    if c.seq == want {
        return;
    }
    c.seq = want;
    c.days = None;
    let (core, dirs, y, m) = (app.core.clone(), shot_dirs(app, &d.dir), c.year, c.month);
    app.spawn(async move { actions::shot_calendar(&core, dirs, y, m).await }, move |app, days| {
        if let Some(c) = app.library.as_mut().and_then(|d| d.calendar.as_mut()).filter(|c| c.seq == want) {
            c.days = Some(days);
        }
    });
}

/// 月曆：一個月一頁，每天的張數與那天的常見詞；點一天就列出那天的截圖
pub fn calendar(app: &mut UiApp, ui: &mut egui::Ui, d: &mut LibraryDialog, rect: Rect) {
    use chrono::Datelike;
    load_calendar(app, d);
    let p = theme::pal(ui);
    let ui = &mut ui.new_child(UiBuilder::new().max_rect(rect).id_salt("calendar"));
    let Some(c) = &mut d.calendar else { return };
    let mut pick: Option<String> = None;
    ui.horizontal(|ui| {
        if Btn::icon_only(Icon::ChevL).ghost().small().tooltip(tr!("上個月", "Previous month")).show(ui).clicked() {
            (c.year, c.month) = if c.month == 1 { (c.year - 1, 12) } else { (c.year, c.month - 1) };
        }
        ui.label(RichText::new(trf!("{} 年 {} 月", "{1}/{0}", c.year, c.month)).font(theme::font_bold(16.0)));
        if Btn::icon_only(Icon::ChevR).ghost().small().tooltip(tr!("下個月", "Next month")).show(ui).clicked() {
            (c.year, c.month) = if c.month == 12 { (c.year + 1, 1) } else { (c.year, c.month + 1) };
        }
        if Btn::new(tr!("這個月", "This month")).ghost().small().show(ui).clicked() {
            let t = CalView::this_month();
            (c.year, c.month) = (t.year, t.month);
        }
        let total: usize = c.days.as_ref().map(|x| x.iter().map(|d| d.count).sum()).unwrap_or(0);
        ui.label(theme::muted(ui, trf!("這個月 {total} 張；點一天就列出那天的截圖", "{total} this month. Click a day to list its screenshots")));
    });
    ui.add_space(6.0);
    let top = ui.cursor().min.y;
    let area = Rect::from_min_max(pos2(rect.min.x, top), rect.max);
    let heads = tr!(["一", "二", "三", "四", "五", "六", "日"], ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]);
    let cw = area.width() / 7.0;
    for (i, h) in heads.iter().enumerate() {
        ui.painter().text(pos2(area.min.x + cw * (i as f32 + 0.5), area.min.y + 9.0), Align2::CENTER_CENTER, *h, theme::font(12.0), p.muted);
    }
    let Some(first) = chrono::NaiveDate::from_ymd_opt(c.year, c.month, 1) else { return };
    let offset = first.weekday().num_days_from_monday() as usize;
    let days_in = (28..=31).rev().find(|d| chrono::NaiveDate::from_ymd_opt(c.year, c.month, *d).is_some()).unwrap_or(28) as usize;
    let weeks = (offset + days_in).div_ceil(7);
    let grid = Rect::from_min_max(pos2(area.min.x, area.min.y + 20.0), area.max);
    let ch = (grid.height() / weeks as f32).min(110.0);
    let today = chrono::Local::now().date_naive();
    let max = c.days.as_ref().and_then(|x| x.iter().map(|d| d.count).max()).unwrap_or(1).max(1);
    for day in 1..=days_in {
        let i = offset + day - 1;
        let cell = Rect::from_min_size(pos2(grid.min.x + cw * (i % 7) as f32, grid.min.y + ch * (i / 7) as f32), vec2(cw, ch)).shrink(3.0);
        let info = c.days.as_ref().and_then(|x| x.iter().find(|d| d.day as usize == day));
        let n = info.map(|d| d.count).unwrap_or(0);
        let resp = ui.interact(cell, Id::new(("calday", day)), if n > 0 { Sense::click() } else { Sense::hover() });
        // 張數越多底色越深
        let fill = if n > 0 { p.accent.gamma_multiply(0.10 + 0.35 * n as f32 / max as f32) } else { p.surface2.gamma_multiply(0.4) };
        ui.painter().rect_filled(cell, CornerRadius::same(6), fill);
        let date = chrono::NaiveDate::from_ymd_opt(c.year, c.month, day as u32);
        let is_today = date == Some(today);
        let border = if resp.hovered() && n > 0 {
            Stroke::new(1.5, p.accent)
        } else if is_today {
            Stroke::new(1.5, p.text)
        } else {
            Stroke::new(1.0, p.border)
        };
        ui.painter().rect_stroke(cell, CornerRadius::same(6), border, egui::StrokeKind::Inside);
        ui.painter().text(cell.min + vec2(7.0, 5.0), Align2::LEFT_TOP, day.to_string(), theme::font_bold(13.0), if n > 0 { p.text } else { p.muted });
        if n > 0 {
            ui.painter().text(pos2(cell.max.x - 7.0, cell.min.y + 6.0), Align2::RIGHT_TOP, trf!("{n} 張", "{n}"), theme::font(11.5), p.text);
            let mut y = cell.min.y + 26.0;
            for t in info.map(|d| d.terms.as_slice()).unwrap_or(&[]) {
                if y + 14.0 > cell.max.y {
                    break;
                }
                let mut job = egui::text::LayoutJob::simple_singleline(t.clone(), theme::font(11.5), p.muted);
                job.wrap = egui::text::TextWrapping::truncate_at_width(cell.width() - 12.0);
                let g = ui.painter().layout_job(job);
                ui.painter().galley(pos2(cell.min.x + 7.0, y), g, p.muted);
                y += 15.0;
            }
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                pick = date.map(|x| x.format("%Y-%m-%d").to_string());
            }
        }
    }
    if c.days.is_none() {
        ui.painter().text(grid.center(), Align2::CENTER_CENTER, tr!("讀取中…", "Loading…"), theme::font(13.0), p.muted);
    }
    if let Some(day) = pick {
        d.calendar = None;
        d.facet.day = Some(day);
        d.page = 1;
        d.dirty = true;
    }
}

// ───────────── 搜尋建議 ─────────────

/// 搜尋字改了：等一下再找建議
pub fn query_changed(d: &mut LibraryDialog) {
    d.suggest.at = Some(Instant::now());
    d.suggest.open = true;
}

/// 打字停下來 0.2 秒後在背景找建議
pub fn tick(app: &mut UiApp, d: &mut LibraryDialog) {
    if !d.suggest.at.is_some_and(|t| t.elapsed().as_millis() > 200) {
        return;
    }
    d.suggest.at = None;
    let q = d.query.trim().to_string();
    if q.is_empty() {
        d.suggest.list.clear();
        d.suggest.query.clear();
        return;
    }
    let core = app.core.clone();
    let qq = q.clone();
    app.spawn(async move { tokio::task::spawn_blocking(move || actions::shot_suggest(&core, &qq, 8)).await.unwrap_or_default() }, move |app, list| {
        if let Some(d) = app.library.as_mut().filter(|d| d.query.trim() == q) {
            d.suggest.query = q;
            d.suggest.list = list;
        }
    });
}

/// 建議清單顯示中
pub fn showing(d: &LibraryDialog) -> bool {
    let q = d.query.trim();
    d.suggest.open && d.suggest.query == q && d.suggest.list.iter().any(|(t, _)| t != q)
}

/// 搜尋框下面的建議清單（圖裡真的有的詞與張數）；點一個就用它搜尋
pub fn popup(ui: &egui::Ui, d: &mut LibraryDialog, search: &egui::Response) {
    // Response::has_focus 在對話框裡不準，直接看焦點在不在搜尋框
    let focused = ui.ctx().memory(|m| m.focused()) == Some(search.id);
    if !focused && !d.suggest.hover {
        d.suggest.open = false;
    }
    let q = d.query.trim();
    let list: Vec<(String, usize)> = d.suggest.list.iter().filter(|(t, _)| t != q).cloned().collect();
    if !d.suggest.open || list.is_empty() || d.suggest.query != q {
        d.suggest.hover = false;
        return;
    }
    let p = theme::pal(ui);
    let mut pick = None;
    let area = egui::Area::new(Id::new("libSuggest")).order(egui::Order::Tooltip).fixed_pos(search.rect.left_bottom() + vec2(0.0, 2.0)).show(ui.ctx(), |ui| {
        egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, p.border)).corner_radius(CornerRadius::same(6)).inner_margin(4).show(ui, |ui| {
            ui.set_width(search.rect.width() - 8.0);
            ui.label(RichText::new(tr!("圖裡有這些字：", "Words found in your screenshots:")).font(theme::font(11.5)).color(p.muted));
            for (t, n) in &list {
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(r, CornerRadius::same(4), p.surface2);
                }
                ui.painter().text(pos2(r.min.x + 8.0, r.center().y), Align2::LEFT_CENTER, t, theme::font(13.0), p.text);
                ui.painter().text(pos2(r.max.x - 8.0, r.center().y), Align2::RIGHT_CENTER, trf!("{n} 張", "{n}"), theme::font(11.5), p.muted);
                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    pick = Some(t.clone());
                }
            }
        });
    });
    d.suggest.hover = area.response.contains_pointer();
    if let Some(t) = pick {
        d.query = t;
        d.suggest.open = false;
        d.suggest.hover = false;
        d.search_at = None;
        d.page = 1;
        d.dirty = true;
    }
}
