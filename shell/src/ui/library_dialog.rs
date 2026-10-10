//! 全部錄影 / 全部截圖：搜尋、篩選、排序、分頁（每頁筆數依視窗大小決定，不出現捲軸）、勾選多筆移到資源回收筒。
//! 錄影用表格：加速版（含 GIF）以子列列在原檔底下，各自有長度、大小與播放 / 資料夾按鈕；
//! 勾選原檔時連同底下的加速版一起勾選，可再個別取消。
//! 截圖用縮圖格：點縮圖開啟。

use super::dialogs::{base_name, date_labels, day_label, file_name, is_cut_name, is_default_name, time_labels, Ask};
use super::theme::{self, chip, Btn, Icon, Tone};
use super::{EntryAction, UiApp};
use eframe::egui::{self, pos2, vec2, Align, Color32, CornerRadius, Id, Layout, Rect, RichText, Sense, Stroke, UiBuilder};
use screenrecorder_core::actions;
use screenrecorder_core::format::{check_recording_name, format_bytes, human_duration, speed_label, video_clock};
use screenrecorder_core::types::{ExportFormat, ExportInfo, LibraryEntry, LibraryFilter, LibraryPage, LibraryQuery, LibrarySort, LIBRARY_ROW_MAIN_PX, LIBRARY_ROW_SUB_PX};
use std::collections::HashSet;
use std::time::Instant;

/// 看錄影還是截圖
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Video,
    Shot,
}

pub struct LibraryDialog {
    /// 需要重新讀取（開啟、換頁、轉檔完成）
    pub dirty: bool,
    kind: Kind,
    query: String,
    filter: LibraryFilter,
    sort: LibrarySort,
    page: usize,
    data: Option<LibraryPage>,
    loading: bool,
    error: Option<String>,
    selected: HashSet<String>,
    search_at: Option<Instant>,
    /// 視窗大小變了（每頁筆數跟著變）：停下來 0.25 秒再重新讀，拖曳調整大小時不會一直掃資料夾
    fit_at: Option<Instant>,
    fit_px: f64,
    /// 截圖格一頁放幾張（依對話框大小）
    grid_per_page: usize,
    seq: u64,
    focus_search: bool,
}

impl LibraryDialog {
    pub fn new(kind: Kind) -> Self {
        Self {
            dirty: true,
            kind,
            query: String::new(),
            filter: LibraryFilter::All,
            sort: LibrarySort::New,
            page: 1,
            data: None,
            loading: false,
            error: None,
            selected: HashSet::new(),
            search_at: None,
            fit_at: None,
            fit_px: 0.0,
            grid_per_page: 0,
            seq: 0,
            focus_search: true,
        }
    }

    /// 勾選 / 取消一個檔案；原檔連同底下的加速版一起（之後可個別取消加速版）
    fn toggle(&mut self, path: &str, on: bool) {
        let subs: Vec<String> = self.data.as_ref().and_then(|d| d.items.iter().find(|e| e.media.path == path)).map(|e| e.exports.iter().map(|x| x.media.path.clone()).collect()).unwrap_or_default();
        for p in std::iter::once(path.to_string()).chain(subs) {
            if on {
                self.selected.insert(p);
            } else {
                self.selected.remove(&p);
            }
        }
    }
}

/// 匯出檔的標籤：4×、GIF 4×、GIF
pub fn export_tag(x: &ExportInfo) -> String {
    if x.format == Some(ExportFormat::Gif) {
        if x.speed > 1.0 {
            format!("GIF {}×", speed_label(x.speed))
        } else {
            "GIF".into()
        }
    } else if x.speed <= 1.0 {
        "壓縮版".into()
    } else {
        format!("{}×", speed_label(x.speed))
    }
}

/// 子列的名稱：4× 加速版、GIF 4×、GIF（原速）。
/// 「指定長度」做出來的倍率是算出來的小數（例如 13.69×），不好讀，改用長度：30 秒版、GIF 30 秒版
fn export_label(x: &ExportInfo) -> String {
    let gif = x.format == Some(ExportFormat::Gif);
    if (x.speed * 2.0).fract() != 0.0 {
        if let Some(d) = x.media.duration_sec {
            let len = format!("{}版", human_duration(d.round()));
            return if gif { format!("GIF {len}") } else { len };
        }
    }
    if gif {
        if x.speed > 1.0 {
            format!("GIF {}×", speed_label(x.speed))
        } else {
            "GIF（原速）".into()
        }
    } else {
        format!("{}× 加速版", speed_label(x.speed))
    }
}

fn load(app: &mut UiApp) {
    let Some(d) = app.library.as_mut() else {
        return;
    };
    d.dirty = false;
    d.loading = true;
    d.seq += 1;
    let seq = d.seq;
    let q = Some(d.query.clone()).filter(|q| !q.trim().is_empty());
    let query = match d.kind {
        Kind::Video => LibraryQuery { q, filter: d.filter, sort: d.sort, page: Some(d.page as f64), page_size: None, fit_px: Some(d.fit_px) },
        Kind::Shot => LibraryQuery { q, filter: LibraryFilter::Shot, sort: d.sort, page: Some(d.page as f64), page_size: Some(d.grid_per_page.max(1) as f64), fit_px: None },
    };
    let dir = app.s.out_dir(&app.env);
    let core = app.core.clone();
    app.spawn(async move { actions::library(&core, &dir, &query).await }, move |app, page| {
        let Some(d) = app.library.as_mut() else {
            return;
        };
        if seq != d.seq {
            return; // 已有較新的查詢（例如還在輸入搜尋字）
        }
        d.loading = false;
        d.page = page.page.max(1);
        for e in &page.items {
            app.known.insert(e.media.path.clone(), e.clone());
        }
        d.data = Some(page);
    });
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let screen = ctx.content_rect();
    let size = vec2((screen.width() - 80.0).min(1040.0), screen.height() - 80.0);
    let mut close = false;
    let mut action: Option<(EntryAction, LibraryEntry)> = None;
    let mut rename: Option<LibraryEntry> = None;
    let mut delete = false;
    let mut combine: Option<bool> = None;
    let mut merge: Option<Vec<String>> = None;
    let dir = app.s.out_dir(&app.env);
    // 縮圖要用 app，先取出對話框
    let Some(mut d) = app.library.take() else {
        return;
    };
    let modal = egui::Modal::new(Id::new("library")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        let p = theme::pal(ui);
        ui.set_width(size.x - 40.0);
        ui.set_height(size.y - 40.0);
        let shot = d.kind == Kind::Shot;
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(if shot { "全部截圖" } else { "全部錄影" }).font(theme::font_bold(17.0)));
                ui.label(RichText::new(&dir).font(theme::mono(12.0)).color(p.muted));
            });
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if Btn::icon_only(Icon::Close).ghost().tooltip("關閉（Esc）").show(ui).clicked() {
                    close = true;
                }
                ui.add_space(8.0);
                // 由右往左排：畫面上是「錄影｜截圖」
                if theme::segmented(ui, &mut d.kind, &[(Kind::Shot, "截圖"), (Kind::Video, "錄影")], true) {
                    d.filter = LibraryFilter::All;
                    if d.sort == LibrarySort::Duration {
                        d.sort = LibrarySort::New;
                    }
                    d.page = 1;
                    d.selected.clear();
                    d.data = None;
                    d.dirty = true;
                }
            });
        });
        let shot = d.kind == Kind::Shot;
        ui.add_space(8.0);
        // 搜尋、篩選、排序
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut d.query).hint_text("搜尋檔名或日期，例如 2026-10-06").desired_width(320.0).min_size(vec2(0.0, 30.0)).vertical_align(Align::Center));
            if d.focus_search {
                r.request_focus();
                d.focus_search = false;
            }
            if r.changed() {
                d.search_at = Some(Instant::now());
            }
            if !shot {
                let filters =
                    [(LibraryFilter::All, "全部"), (LibraryFilter::Original, "原始錄影"), (LibraryFilter::Cut, "剪輯版"), (LibraryFilter::Speed, "有加速版"), (LibraryFilter::Audio, "有聲音")];
                egui::ComboBox::from_id_salt("libFilter").selected_text(filters.iter().find(|f| f.0 == d.filter).map(|f| f.1).unwrap_or("")).show_ui(ui, |ui| {
                    for (v, t) in filters {
                        if ui.selectable_label(d.filter == v, t).clicked() {
                            d.filter = v;
                            d.page = 1;
                            d.dirty = true;
                        }
                    }
                });
            }
            let sorts = [(LibrarySort::New, "最新的在前"), (LibrarySort::Old, "最舊的在前"), (LibrarySort::Duration, "長度（長到短）"), (LibrarySort::Size, "檔案大小（大到小）")];
            let sorts: Vec<_> = sorts.into_iter().filter(|s| !(shot && s.0 == LibrarySort::Duration)).collect();
            egui::ComboBox::from_id_salt("libSort").selected_text(sorts.iter().find(|s| s.0 == d.sort).map(|s| s.1).unwrap_or("")).show_ui(ui, |ui| {
                for (v, t) in sorts {
                    if ui.selectable_label(d.sort == v, t).clicked() {
                        d.sort = v;
                        d.page = 1;
                        d.dirty = true;
                    }
                }
            });
        });
        if d.search_at.is_some_and(|t| t.elapsed().as_millis() > 250) {
            d.search_at = None;
            d.page = 1;
            d.dirty = true;
        }
        ui.add_space(8.0);
        let foot_h = 40.0;
        let table = Rect::from_min_max(ui.cursor().min, pos2(ui.max_rect().max.x, ui.max_rect().max.y - foot_h - 8.0));
        if shot {
            shot_grid(app, ui, &mut d, table, &mut action, &mut rename);
        } else {
            video_table(app, ui, &mut d, table, &mut action, &mut rename);
        }
        // 下方：選取與分頁
        let painter = ui.painter().clone();
        let foot = Rect::from_min_max(pos2(table.min.x, ui.max_rect().max.y - foot_h), ui.max_rect().max);
        painter.hline(foot.x_range(), foot.min.y - 4.0, Stroke::new(1.0, p.border));
        let fui = &mut ui.new_child(UiBuilder::new().max_rect(foot).layout(Layout::left_to_right(Align::Center)));
        let n = d.selected.len();
        fui.label(theme::muted(
            fui,
            match (n, shot) {
                (0, true) => "可勾選多張一次刪除，或拼成一張".into(),
                (0, false) => "可勾選多筆一次刪除".into(),
                _ => format!("已選取 {n} 個檔案"),
            },
        ));
        if n > 0 && Btn::new("移到資源回收筒").icon(Icon::Trash).danger().small().show(fui).clicked() {
            delete = true;
        }
        let mp4s: Vec<String> = d.selected.iter().filter(|p| p.to_lowercase().ends_with(".mp4")).cloned().collect();
        if !shot && mp4s.len() >= 2 && Btn::new("合併成一支").small().tooltip("把勾選的錄影依時間順序接成一支（大小以最早的那支為準）；GIF 不算").show(fui).clicked()
        {
            merge = Some(mp4s);
        }
        if shot && n >= 2 {
            let b = Btn::new("拼成一張").small().tooltip("把勾選的截圖依時間順序拼成一張新的截圖").show(fui);
            egui::Popup::menu(&b).show(|ui| {
                ui.set_min_width(150.0);
                if ui.button("左右排").clicked() {
                    combine = Some(false);
                }
                if ui.button("上下排").clicked() {
                    combine = Some(true);
                }
            });
        }
        fui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let pages = d.data.as_ref().map(|x| x.pages).unwrap_or(1);
            if Btn::icon_only(Icon::ChevR).ghost().small().enabled(d.page < pages).tooltip("下一頁").show(ui).clicked() {
                d.page += 1;
                d.dirty = true;
            }
            if let Some(x) = &d.data {
                ui.label(RichText::new(format!("第 {} / {} 頁・共 {} 筆", d.page, x.pages.max(1), x.total)).font(theme::font(12.5)));
            }
            if Btn::icon_only(Icon::ChevL).ghost().small().enabled(d.page > 1).tooltip("上一頁").show(ui).clicked() {
                d.page -= 1;
                d.dirty = true;
            }
        });
    });
    if modal.should_close() && app.ask.is_none() {
        close = true;
    }
    if close {
        return;
    }
    let dirty = d.dirty;
    app.library = Some(d);
    if dirty {
        load(app);
    }
    if let Some((act, e)) = action {
        // 剪輯影片、製作加速版時關掉清單；編輯截圖蓋在清單上，關掉編輯就回到清單
        if matches!(act, EntryAction::Edit | EntryAction::Export) && !super::dialogs::is_image(&e.media.path) {
            app.library = None;
        }
        app.act(act, e);
    }
    if let Some(e) = rename {
        rename_entry(app, e);
    }
    if delete {
        remove_selected(app);
    }
    if let Some(paths) = merge {
        let core = app.core.clone();
        let n = paths.len();
        app.spawn(async move { actions::merge_start(&core, &paths).await }, move |app, r| match r {
            Ok(()) => {
                if let Some(d) = &mut app.library {
                    d.selected.clear();
                }
                app.toast(format!("開始合併 {n} 支錄影，完成後會出現在清單"), false);
            }
            Err(e) => app.toast(e.message().to_string(), true),
        });
    }
    if let Some(vertical) = combine {
        let paths: Vec<String> = app.library.as_ref().map(|d| d.selected.iter().cloned().collect()).unwrap_or_default();
        let core = app.core.clone();
        app.spawn(async move { core.combine_shots(paths, vertical).await }, |app, r| match r {
            Ok(shot) => {
                if let Some(d) = &mut app.library {
                    d.selected.clear();
                }
                // 狀態更新的「已截圖」不用再顯示一次
                app.last_shot_seq = app.last_shot_seq.max(shot.seq);
                app.toast(format!("已拼成 {}（{}×{}）{}", super::dialogs::file_name(&shot.path), shot.width, shot.height, if shot.copied { "，並複製到剪貼簿" } else { "" }), false);
                app.shots_changed();
            }
            Err(e) => app.toast(e.message().to_string(), true),
        });
    }
}

/// 錄影：表格（高度固定，伺服器依每筆的子列數分頁，一頁剛好放滿）。
/// 依日期分組：每天第一筆在「日期」欄寫「今天」「昨天」「10/08（三）」，其餘只寫時間
fn video_table(app: &mut UiApp, ui: &mut egui::Ui, d: &mut LibraryDialog, table: Rect, action: &mut Option<(EntryAction, LibraryEntry)>, rename: &mut Option<LibraryEntry>) {
    let p = theme::pal(ui);
    let head_h = 30.0;
    let fit = ((table.height() - head_h - 4.0) as f64).max(LIBRARY_ROW_MAIN_PX * 3.0).floor();
    if (fit - d.fit_px).abs() > 0.5 {
        d.fit_px = fit;
        if d.data.is_none() {
            d.dirty = true;
        } else {
            d.fit_at = Some(Instant::now());
        }
    }
    if d.fit_at.is_some_and(|t| t.elapsed().as_millis() > 250) {
        d.fit_at = None;
        d.dirty = true;
    } else if d.fit_at.is_some() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(260));
    }
    let rows = d.data.as_ref().map(|x| x.items.clone()).unwrap_or_default();
    // 勾選、日期、檔案、長度、解析度、大小、操作
    let cols = [36.0, 104.0, 0.0, 84.0, 104.0, 90.0, 136.0];
    let name_w = table.width() - cols.iter().sum::<f32>();
    let col_x = |i: usize| -> f32 {
        let widths = [cols[0], cols[1], name_w, cols[3], cols[4], cols[5], cols[6]];
        table.min.x + widths[..i].iter().sum::<f32>()
    };
    let painter = ui.painter().clone();
    let num = |t: &str, x: f32, y: f32, color: Color32, size: f32| {
        painter.text(pos2(x, y), egui::Align2::RIGHT_CENTER, t, theme::font(size), color);
    };
    painter.rect_filled(Rect::from_min_size(table.min, vec2(table.width(), head_h)), CornerRadius::same(6), p.surface2);
    // 標題列（勾選框與每一列的對齊）
    {
        let head = Rect::from_min_size(table.min, vec2(table.width(), head_h));
        let all = !rows.is_empty() && rows.iter().all(|e| d.selected.contains(&e.media.path) && e.exports.iter().all(|x| d.selected.contains(&x.media.path)));
        let mut all2 = all;
        let ui2 = &mut ui
            .new_child(UiBuilder::new().max_rect(Rect::from_min_max(head.min + vec2(10.0, 0.0), pos2(head.min.x + col_x(1) - table.min.x, head.max.y))).layout(Layout::left_to_right(Align::Center)));
        if ui2.checkbox(&mut all2, "").on_hover_text("全選本頁").changed() {
            for e in &rows {
                d.toggle(&e.media.path, all2);
            }
        }
        for (i, (t, right)) in [("日期", false), ("檔案", false), ("長度", true), ("解析度", true), ("大小", true)].iter().enumerate() {
            let x0 = col_x(i + 1);
            let x1 = col_x(i + 2);
            let (pos, align) = if *right { (pos2(x1 - 10.0, head.center().y), egui::Align2::RIGHT_CENTER) } else { (pos2(x0 + 6.0, head.center().y), egui::Align2::LEFT_CENTER) };
            painter.text(pos, align, *t, theme::font_bold(12.5), p.muted);
        }
    }
    // 內容
    let mut y = table.min.y + head_h + 2.0;
    let times = time_labels(rows.iter().map(|e| (e.media.name.as_str(), e.media.mtime)));
    let today = chrono::Local::now().date_naive();
    let mut last_day = None;
    if rows.is_empty() {
        let msg = if d.loading {
            "讀取中…".to_string()
        } else if let Some(e) = &d.error {
            e.clone()
        } else if d.data.as_ref().is_some_and(|x| x.total == 0) && d.query.trim().is_empty() {
            "這個資料夾還沒有錄影。".into()
        } else {
            "沒有符合條件的錄影。".into()
        };
        painter.text(pos2(table.center().x, table.min.y + 120.0), egui::Align2::CENTER_CENTER, msg, theme::font(14.0), p.muted);
    }
    for e in &rows {
        let row = Rect::from_min_size(pos2(table.min.x, y), vec2(table.width(), LIBRARY_ROW_MAIN_PX as f32));
        let resp = ui.interact(row, Id::new(("librow", &e.media.path)), Sense::hover());
        // 子列（加速版）也算在這一列的範圍內：滑鼠在子列上時，原片的按鈕也亮起來
        let group = Rect::from_min_size(row.min, vec2(row.width(), LIBRARY_ROW_MAIN_PX as f32 + e.exports.len() as f32 * LIBRARY_ROW_SUB_PX as f32));
        let hot = ui.rect_contains_pointer(group);
        if resp.hovered() {
            painter.rect_filled(row, CornerRadius::same(4), p.surface2.gamma_multiply(0.6));
        }
        let (day, time) = times.get(&e.media.name).cloned().unzip();
        // 新的一天：上方用深一點的線分隔，日期欄寫日期
        let new_day = day.is_some() && day != last_day;
        if new_day && last_day.is_some() {
            painter.hline(row.x_range(), row.min.y - 1.0, Stroke::new(1.0, p.border_strong));
        }
        last_day = day;
        painter.hline(row.x_range(), row.max.y, Stroke::new(1.0, p.border));
        let rui = &mut ui.new_child(UiBuilder::new().max_rect(row).layout(Layout::left_to_right(Align::Center)));
        rui.add_space(10.0);
        let mut on = d.selected.contains(&e.media.path);
        if rui.checkbox(&mut on, "").changed() {
            d.toggle(&e.media.path, on);
        }
        if new_day {
            if let Some(dd) = day {
                painter.text(pos2(col_x(1) + 6.0, row.center().y), egui::Align2::LEFT_CENTER, day_label(dd, today), theme::font_bold(13.0), p.text);
            }
        }
        // 檔案欄：縮圖、時間、自訂名稱、剪輯版、有聲音
        let name_rect = Rect::from_min_max(pos2(col_x(2), row.min.y), pos2(col_x(3), row.max.y));
        let thumb = Rect::from_min_size(pos2(name_rect.min.x + 4.0, row.center().y - 15.0), vec2(53.0, 30.0));
        painter.rect_filled(thumb, CornerRadius::same(4), p.surface2);
        if let Some(t) = app.thumb(&e.media.path, e.media.mtime) {
            theme::paint_thumb(&painter, thumb, &t);
        }
        {
            let nui = &mut ui.new_child(
                UiBuilder::new().max_rect(Rect::from_min_max(pos2(thumb.max.x + 10.0, row.min.y), pos2(name_rect.max.x - 8.0, name_rect.max.y))).layout(Layout::left_to_right(Align::Center)),
            );
            nui.label(RichText::new(time.unwrap_or_default()).font(theme::font_bold(13.0)));
            if !is_default_name(&e.media.name) {
                nui.add(egui::Label::new(RichText::new(base_name(&e.media.name)).font(theme::font(13.0))).truncate());
            }
            if is_cut_name(&e.media.name) {
                chip(nui, "剪輯版", Tone::Warn, false);
            }
            if e.media.has_audio == Some(true) {
                audio_mark(nui);
            }
        }
        num(&e.media.duration_sec.map(video_clock).unwrap_or_else(|| "—".into()), col_x(4) - 10.0, row.center().y, p.text, 13.0);
        let res = match (e.media.width, e.media.height) {
            (Some(w), Some(h)) => format!("{w}×{h}"),
            _ => "—".into(),
        };
        num(&res, col_x(5) - 10.0, row.center().y, p.muted, 12.5);
        num(&format_bytes(e.media.bytes), col_x(6) - 10.0, row.center().y, p.text, 13.0);
        {
            let aui = &mut ui.new_child(UiBuilder::new().max_rect(Rect::from_min_max(pos2(col_x(6), row.min.y), row.max)).layout(Layout::left_to_right(Align::Center)));
            aui.spacing_mut().item_spacing.x = 2.0;
            for (act, icon, tip) in [(EntryAction::Play, Icon::Play, "播放"), (EntryAction::Reveal, Icon::Folder, "在資料夾中顯示"), (EntryAction::Edit, Icon::Cut, "剪輯")] {
                if Btn::icon_only(icon).ghost().small().quiet(!hot).tooltip(tip).show(aui).clicked() {
                    *action = Some((act, e.clone()));
                }
            }
            // 不常用的收進「⋯」
            let more = Btn::icon_only(Icon::More).ghost().small().quiet(!hot).tooltip("更多").show(aui);
            egui::Popup::menu(&more).show(|ui| {
                ui.set_min_width(190.0);
                if ui.button("製作加速版 / GIF").clicked() {
                    *action = Some((EntryAction::Export, e.clone()));
                }
                if ui.button("複製檔案（貼到 LINE、資料夾）").clicked() {
                    *action = Some((EntryAction::CopyFile, e.clone()));
                }
                if e.media.has_audio != Some(false) && ui.button("存成 M4A（只留聲音）").clicked() {
                    *action = Some((EntryAction::SaveAudio, e.clone()));
                }
                if ui.button("重新命名（加速版一起改）").clicked() {
                    *rename = Some(e.clone());
                }
            });
        }
        y += LIBRARY_ROW_MAIN_PX as f32;
        // 加速版 / GIF 子列
        for x in &e.exports {
            let row = Rect::from_min_size(pos2(table.min.x, y), vec2(table.width(), LIBRARY_ROW_SUB_PX as f32));
            painter.hline(row.x_range(), row.max.y, Stroke::new(1.0, p.border.gamma_multiply(0.6)));
            let sui = &mut ui.new_child(UiBuilder::new().max_rect(row).layout(Layout::left_to_right(Align::Center)));
            sui.add_space(10.0);
            let mut on = d.selected.contains(&x.media.path);
            if sui.checkbox(&mut on, "").changed() {
                if on {
                    d.selected.insert(x.media.path.clone());
                } else {
                    d.selected.remove(&x.media.path);
                }
            }
            // 樹狀線
            let bx = col_x(2) + 30.0;
            painter.line_segment([pos2(bx, row.min.y - 6.0), pos2(bx, row.center().y)], Stroke::new(1.0, p.border_strong));
            painter.line_segment([pos2(bx, row.center().y), pos2(bx + 12.0, row.center().y)], Stroke::new(1.0, p.border_strong));
            {
                let nui = &mut ui.new_child(UiBuilder::new().max_rect(Rect::from_min_max(pos2(bx + 18.0, row.min.y), pos2(col_x(3), row.max.y))).layout(Layout::left_to_right(Align::Center)));
                chip(nui, &export_label(x), Tone::Accent, false).on_hover_text(format!("{}（{}×）", x.media.name, speed_label(x.speed)));
            }
            num(&x.media.duration_sec.map(video_clock).unwrap_or_else(|| "—".into()), col_x(4) - 10.0, row.center().y, p.text, 12.5);
            let res = match (x.media.width, x.media.height) {
                (Some(w), Some(h)) => format!("{w}×{h}"),
                _ => "—".into(),
            };
            num(&res, col_x(5) - 10.0, row.center().y, p.muted, 12.0);
            num(&format_bytes(x.media.bytes), col_x(6) - 10.0, row.center().y, p.text, 12.5);
            let aui = &mut ui.new_child(UiBuilder::new().max_rect(Rect::from_min_max(pos2(col_x(6), row.min.y), row.max)).layout(Layout::left_to_right(Align::Center)));
            aui.spacing_mut().item_spacing.x = 2.0;
            for (act, icon, tip) in [(EntryAction::Play, Icon::Play, "播放"), (EntryAction::Reveal, Icon::Folder, "在資料夾中顯示")] {
                if Btn::icon_only(icon).ghost().small().quiet(!hot).tooltip(tip).show(aui).clicked() {
                    *action = Some((act, LibraryEntry { media: x.media.clone(), exports: vec![] }));
                }
            }
            y += LIBRARY_ROW_SUB_PX as f32;
        }
    }
}

/// 「有聲音」：灰色小喇叭（滑鼠提示說明），不用彩色標籤
pub fn audio_mark(ui: &mut egui::Ui) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
    theme::paint_icon(ui.painter(), r, Icon::Speaker, theme::pal(ui).muted);
    resp.on_hover_text("有聲音")
}

/// 截圖：縮圖格（一頁放幾張依對話框大小），點縮圖開啟
fn shot_grid(app: &mut UiApp, ui: &mut egui::Ui, d: &mut LibraryDialog, area: Rect, action: &mut Option<(EntryAction, LibraryEntry)>, rename: &mut Option<LibraryEntry>) {
    let p = theme::pal(ui);
    let gap = 14.0;
    let cols = ((area.width() + gap) / (210.0 + gap)).floor().max(1.0);
    let cell_w = (area.width() - gap * (cols - 1.0)) / cols;
    let img_h = (cell_w * 9.0 / 16.0).round();
    let cell_h = img_h + 64.0;
    let rows = ((area.height() + gap) / (cell_h + gap)).floor().max(1.0);
    let per = (cols * rows) as usize;
    if per != d.grid_per_page {
        d.grid_per_page = per;
        d.dirty = true;
    }
    let items = d.data.as_ref().map(|x| x.items.clone()).unwrap_or_default();
    let painter = ui.painter().clone();
    if items.is_empty() {
        let msg = if d.loading {
            "讀取中…".to_string()
        } else if let Some(e) = &d.error {
            e.clone()
        } else if d.data.as_ref().is_some_and(|x| x.total == 0) && d.query.trim().is_empty() {
            match app.keys.label(2) {
                k if k.is_empty() => "還沒有截圖。按主畫面的「截圖」試試看。".to_string(),
                k => format!("還沒有截圖。按主畫面的「截圖」或 {k} 試試看。"),
            }
        } else {
            "沒有符合條件的截圖。".into()
        };
        painter.text(pos2(area.center().x, area.min.y + 120.0), egui::Align2::CENTER_CENTER, msg, theme::font(14.0), p.muted);
    }
    let dates = date_labels(items.iter().map(|e| (e.media.name.as_str(), e.media.mtime)));
    for (i, e) in items.iter().enumerate() {
        let (c, r) = ((i % cols as usize) as f32, (i / cols as usize) as f32);
        let cell = Rect::from_min_size(pos2(area.min.x + c * (cell_w + gap), area.min.y + r * (cell_h + gap)), vec2(cell_w, cell_h));
        let hot = ui.rect_contains_pointer(cell);
        let on = d.selected.contains(&e.media.path);
        let img = Rect::from_min_size(cell.min, vec2(cell_w, img_h));
        let resp = ui.interact(img, Id::new(("shotcell", &e.media.path)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(format!("{}\n點一下開啟", e.media.name));
        let border = if on {
            Stroke::new(2.0, p.accent)
        } else if resp.hovered() {
            Stroke::new(1.0, p.border_strong)
        } else {
            Stroke::new(1.0, p.border)
        };
        painter.rect_filled(img, CornerRadius::same(theme::RADIUS_SM), p.surface2);
        if let Some(t) = app.thumb(&e.media.path, e.media.mtime) {
            theme::paint_thumb(&painter.with_clip_rect(img.shrink(1.0)), img.shrink(4.0), &t);
        }
        painter.rect_stroke(img, CornerRadius::same(theme::RADIUS_SM), border, egui::StrokeKind::Inside);
        if resp.clicked() {
            *action = Some((EntryAction::Play, e.clone()));
        }
        // 左上角勾選（白底，在深色縮圖上也看得到）
        let cb = Rect::from_min_size(img.min + vec2(6.0, 6.0), vec2(24.0, 22.0));
        if on || resp.hovered() || ui.rect_contains_pointer(cb) {
            painter.rect_filled(cb, CornerRadius::same(5), p.surface.gamma_multiply(0.92));
        }
        {
            let cui = &mut ui.new_child(UiBuilder::new().max_rect(cb.shrink2(vec2(4.0, 1.0))));
            let mut v = on;
            if cui.checkbox(&mut v, "").changed() {
                d.toggle(&e.media.path, v);
            }
        }
        // 名稱（預設檔名顯示日期時間）
        let title = if is_default_name(&e.media.name) { dates.get(&e.media.name).cloned().unwrap_or_default() } else { base_name(&e.media.name) };
        let tr = Rect::from_min_max(pos2(cell.min.x + 2.0, img.max.y + 6.0), pos2(cell.max.x, img.max.y + 28.0));
        ui.new_child(UiBuilder::new().max_rect(tr).layout(Layout::left_to_right(Align::Center))).add(egui::Label::new(RichText::new(title).font(theme::font_bold(13.0))).truncate());
        // 解析度・大小，右邊放按鈕
        let br = Rect::from_min_max(pos2(cell.min.x + 2.0, img.max.y + 30.0), cell.max);
        let bui = &mut ui.new_child(UiBuilder::new().max_rect(br).layout(Layout::left_to_right(Align::Center)));
        let size = e.media.width.zip(e.media.height).map(|(w, h)| format!("{w}×{h}・")).unwrap_or_default();
        bui.label(RichText::new(format!("{size}{}", format_bytes(e.media.bytes))).font(theme::font(12.0)).color(p.muted));
        bui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if Btn::icon_only(Icon::Edit).ghost().small().quiet(!hot).tooltip("重新命名").show(ui).clicked() {
                *rename = Some(e.clone());
            }
            if Btn::icon_only(Icon::Folder).ghost().small().quiet(!hot).tooltip("在資料夾中顯示").show(ui).clicked() {
                *action = Some((EntryAction::Reveal, e.clone()));
            }
            if Btn::icon_only(Icon::Cut).ghost().small().quiet(!hot).tooltip("編輯：標註、遮個資、裁切（另存一張）").show(ui).clicked() {
                *action = Some((EntryAction::Edit, e.clone()));
            }
            if Btn::icon_only(Icon::Eye).ghost().small().quiet(!hot).tooltip("檢視").show(ui).clicked() {
                *action = Some((EntryAction::Play, e.clone()));
            }
        });
    }
}

/// 改名：原片與底下的加速版一起改；送出時檢查衝突、使用中，錯誤留在對話框裡
fn rename_entry(app: &mut UiApp, entry: LibraryEntry) {
    let old = base_name(&entry.media.name);
    let n = entry.exports.len();
    let mut ask = Ask::input("重新命名", "新名稱", old.clone(), "改名", move |app, value| {
        let name = value.trim().trim_end_matches(".mp4").trim_end_matches(".MP4").trim_end_matches(".png").trim_end_matches(".PNG").to_string();
        if name == old {
            app.ask = None;
            return;
        }
        if let Some(bad) = check_recording_name(&name) {
            if let Some(a) = &mut app.ask {
                a.error = Some(bad.to_string());
                a.on_ok = None;
            }
            // 重新允許送出
            rename_retry(app, entry.clone());
            return;
        }
        if let Some(a) = &mut app.ask {
            a.busy = true;
        }
        let core = app.core.clone();
        let path = entry.media.path.clone();
        let e2 = entry.clone();
        app.spawn(async move { actions::rename(&core, &path, &value).await }, move |app, r| match r {
            Ok(_) => {
                app.ask = None;
                app.toast(format!("已改名為 {name}"), false);
                if let Some(d) = &mut app.library {
                    d.selected.remove(&e2.media.path);
                    for x in &e2.exports {
                        d.selected.remove(&x.media.path);
                    }
                    d.dirty = true;
                }
                app.load_recent();
            }
            Err(err) => {
                if let Some(a) = &mut app.ask {
                    a.busy = false;
                    a.error = Some(err.message().to_string());
                }
                rename_retry(app, e2);
            }
        });
    });
    if n > 0 {
        ask.message = Some(format!("底下的 {n} 個加速版會一起改名。"));
    }
    app.ask = Some(ask);
}

/// 改名失敗後：保留對話框（含輸入的文字與錯誤訊息），讓使用者再送出一次
fn rename_retry(app: &mut UiApp, entry: LibraryEntry) {
    let Some(a) = app.ask.take() else { return };
    let value = a.input.clone().unwrap_or_default();
    let error = a.error.clone();
    rename_entry(app, entry);
    if let Some(n) = &mut app.ask {
        n.input = Some(value);
        n.error = error;
    }
}

fn remove_selected(app: &mut UiApp) {
    let Some(d) = &app.library else { return };
    let list: Vec<String> = d.selected.iter().cloned().collect();
    // 原檔要刪、但取消勾選了部分加速版：提醒這些會保留
    let kept: usize =
        d.data.as_ref().map(|x| x.items.iter().filter(|e| d.selected.contains(&e.media.path)).map(|e| e.exports.iter().filter(|x| !d.selected.contains(&x.media.path)).count()).sum()).unwrap_or(0);
    let mut ask = Ask::confirm(
        format!("把 {} 個檔案移到資源回收筒？", list.len()),
        format!("可從資源回收筒還原。{}", if kept > 0 { format!("\n未勾選的 {kept} 個加速版會保留。") } else { String::new() }),
        "移到資源回收筒",
        move |app, _| {
            app.ask = None;
            match actions::delete(&app.core, &list) {
                Ok(n) => {
                    app.toast(format!("已將 {n} 個檔案移到資源回收筒"), false);
                    if let Some(d) = &mut app.library {
                        d.selected.clear();
                        d.dirty = true;
                    }
                    app.load_recent();
                }
                Err(e) => app.toast(e.message().to_string(), true),
            }
        },
    );
    ask.danger = true;
    ask.list = d.selected.iter().map(|p| file_name(p)).collect();
    ask.list.sort();
    app.ask = Some(ask);
}
