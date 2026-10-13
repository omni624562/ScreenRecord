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
use screenrecorder_core::i18n::is_en;
use screenrecorder_core::types::{ExportFormat, ExportInfo, LibraryEntry, LibraryFilter, LibraryPage, LibraryQuery, LibrarySort, LIBRARY_HITS_SHOWN, LIBRARY_ROW_MAIN_PX, LIBRARY_ROW_SUB_PX};
use screenrecorder_core::{tr, trf};
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
    /// 看哪個資料夾：None = 目前的儲存位置；也可以看之前用過的資料夾
    dir: Option<String>,
    /// 上次讀取的資料夾（儲存位置改了就重新讀取）
    loaded_dir: String,
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
    /// 點了說過的話：開啟影片並跳到那一句
    jump: Option<(LibraryEntry, f64)>,
}

impl LibraryDialog {
    pub fn new(kind: Kind) -> Self {
        Self {
            dirty: true,
            kind,
            dir: None,
            loaded_dir: String::new(),
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
            jump: None,
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

/// 動圖的名稱（GIF、WebP）；不是動圖時 None
fn anim_name(x: &ExportInfo) -> Option<&'static str> {
    match x.format {
        Some(ExportFormat::Gif) => Some("GIF"),
        Some(ExportFormat::Webp) => Some("WebP"),
        _ => None,
    }
}

/// 匯出檔的標籤：4×、GIF 4×、GIF
pub fn export_tag(x: &ExportInfo) -> String {
    if let Some(name) = anim_name(x) {
        if x.speed > 1.0 {
            format!("{name} {}×", speed_label(x.speed))
        } else {
            name.into()
        }
    } else if x.speed <= 1.0 {
        tr!("壓縮版", "Compressed").into()
    } else {
        format!("{}×", speed_label(x.speed))
    }
}

/// 子列的名稱：4× 加速版、GIF 4×、GIF（原速）。
/// 「指定長度」做出來的倍率是算出來的小數（例如 13.69×），不好讀，改用長度：30 秒版、GIF 30 秒版
fn export_label(x: &ExportInfo) -> String {
    let gif = anim_name(x).is_some();
    let anim = anim_name(x).unwrap_or("GIF");
    if (x.speed * 2.0).fract() != 0.0 {
        if let Some(d) = x.media.duration_sec {
            let len = trf!("{}版", "{} version", human_duration(d.round()));
            return if gif { format!("{anim} {len}") } else { len };
        }
    }
    if gif {
        if x.speed > 1.0 {
            format!("{anim} {}×", speed_label(x.speed))
        } else {
            trf!("{anim}（原速）", "{anim} (original speed)")
        }
    } else if x.speed <= 1.0 {
        tr!("壓縮版", "Compressed").to_string()
    } else {
        trf!("{}× 加速版", "{}× speed", speed_label(x.speed))
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
    let dir = d.dir.clone().unwrap_or_else(|| app.s.out_dir(&app.env));
    d.loaded_dir = dir.clone();
    let core = app.core.clone();
    app.spawn(async move { actions::library(&core, &dir, &query).await }, move |app, page| {
        let Some(d) = app.library.as_mut() else {
            return;
        };
        if seq != d.seq {
            return; // 已有較新的查詢（例如還在輸入搜尋字）
        }
        d.loading = false;
        d.error = page.error.clone();
        d.page = page.page.max(1);
        for e in &page.items {
            app.known.insert(e.media.path.clone(), e.clone());
        }
        d.data = Some(page);
    });
}

/// 清單上按了什麼（關掉對話框後才處理，處理時要用到 app）
enum Req {
    Entry(EntryAction, LibraryEntry),
    Rename(LibraryEntry),
    /// 勾選的檔案移到資源回收筒
    DeleteSelected,
    /// 換儲存位置（Windows 的選資料夾視窗）
    ChooseDir,
    /// 在檔案總管開啟資料夾
    OpenDir(String),
    /// 看哪個資料夾（None = 目前的儲存位置）
    Browse(Option<String>),
    /// 關掉清單、開始錄影
    Record,
    Merge(Vec<String>),
    Combine(bool),
}

/// 記住用過的儲存位置（最近用的在前；資料夾選單的「之前用過的資料夾」）
pub fn remember_dir(app: &mut UiApp, dir: &str) {
    let dir = dir.trim();
    if dir.is_empty() || app.s.recent_dirs.first().is_some_and(|d| d.eq_ignore_ascii_case(dir)) {
        return;
    }
    app.s.recent_dirs.retain(|d| !d.eq_ignore_ascii_case(dir));
    app.s.recent_dirs.insert(0, dir.to_string());
    app.s.recent_dirs.truncate(super::settings::RECENT_DIRS);
    app.save_settings();
}

/// 太長的路徑從中間省略（保留開頭的磁碟與最後的資料夾名稱）：C:\Users\…\Videos\Timelapse
fn middle_truncate(ui: &egui::Ui, text: &str, font: &egui::FontId, max_w: f32) -> String {
    let width = |t: &str| ui.painter().layout_no_wrap(t.to_string(), font.clone(), Color32::WHITE).size().x;
    if width(text) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let cut = |keep: usize| -> String {
        // 後面保留多一點：最後的資料夾名稱最重要
        let head = keep / 3;
        let tail = keep - head;
        chars[..head].iter().chain(std::iter::once(&'…')).chain(chars[chars.len() - tail..].iter()).collect()
    };
    let (mut lo, mut hi) = (0, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if width(&cut(mid)) <= max_w {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    cut(lo)
}

/// 標題下的資料夾：資料夾圖示與路徑（太長時從中間省略），點一下在檔案總管開啟，滑鼠提示完整路徑
fn folder_link(ui: &mut egui::Ui, dir: &str, max_w: f32) -> egui::Response {
    let p = theme::pal(ui);
    let font = theme::font(12.5);
    let text = middle_truncate(ui, dir, &font, (max_w - 22.0).max(40.0));
    let g = ui.painter().layout_no_wrap(text, font, p.muted);
    let (rect, resp) = ui.allocate_exact_size(vec2(22.0 + g.size().x, 22.0), Sense::click());
    let color = if resp.hovered() { p.accent } else { p.muted };
    theme::paint_icon(ui.painter(), Rect::from_center_size(pos2(rect.min.x + 8.0, rect.center().y), vec2(15.0, 15.0)), Icon::Folder, color);
    let at = pos2(rect.min.x + 22.0, rect.center().y - g.size().y / 2.0);
    if resp.hovered() {
        ui.painter().hline(at.x..=at.x + g.size().x, at.y + g.size().y, Stroke::new(1.0, color));
    }
    ui.painter().galley_with_override_text_color(at, g, color);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(trf!("{dir}\n點一下開啟資料夾", "{dir}\nClick to open the folder"))
}

/// 清單是空的或讀不到資料夾：說明與按鈕（置中）；回傳按了第幾個按鈕
fn empty_state(ui: &mut egui::Ui, area: Rect, msg: &str, buttons: &[(&str, bool)]) -> Option<usize> {
    let p = theme::pal(ui);
    let r = Rect::from_center_size(pos2(area.center().x, area.min.y + 130.0), vec2(area.width().min(640.0), 110.0));
    let ui = &mut ui.new_child(UiBuilder::new().max_rect(r).layout(Layout::top_down(Align::Center)));
    ui.add(egui::Label::new(RichText::new(msg).font(theme::font(14.5)).color(p.muted)).wrap());
    let mut clicked = None;
    if !buttons.is_empty() {
        ui.add_space(10.0);
        // 一排按鈕置中：先量好總寬度
        let gap = 8.0;
        let total: f32 = buttons.iter().map(|(t, _)| Btn::new(*t).width(ui)).sum::<f32>() + gap * (buttons.len() - 1) as f32;
        let row = Rect::from_center_size(pos2(r.center().x, ui.cursor().min.y + 17.0), vec2(total, 34.0));
        let bui = &mut ui.new_child(UiBuilder::new().max_rect(row).layout(Layout::left_to_right(Align::Center)));
        bui.spacing_mut().item_spacing.x = gap;
        for (i, (t, primary)) in buttons.iter().enumerate() {
            let b = Btn::new(*t);
            if (if *primary { b.primary() } else { b }).show(bui).clicked() {
                clicked = Some(i);
            }
        }
    }
    clicked
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let screen = ctx.content_rect();
    let size = vec2((screen.width() - 80.0).min(1040.0), screen.height() - 80.0);
    let mut close = false;
    let mut req: Option<Req> = None;
    // 目前的儲存位置記在「用過的資料夾」最前面（換了儲存位置之後，舊的還能從選單切回去看）
    let save_dir = app.s.out_dir(&app.env);
    remember_dir(app, &save_dir);
    let recent: Vec<String> = app.s.recent_dirs.iter().filter(|d| !d.eq_ignore_ascii_case(&save_dir)).cloned().collect();
    // 縮圖要用 app，先取出對話框
    let Some(mut d) = app.library.take() else {
        return;
    };
    let dir = d.dir.clone().unwrap_or_else(|| save_dir.clone());
    // 儲存位置改了（例如按了「變更儲存位置…」）：重新讀取
    if !d.loading && !d.dirty && !d.loaded_dir.is_empty() && d.loaded_dir != dir {
        d.page = 1;
        d.selected.clear();
        d.data = None;
        d.dirty = true;
    }
    let modal = egui::Modal::new(Id::new("library")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        let p = theme::pal(ui);
        ui.set_width(size.x - 40.0);
        ui.set_height(size.y - 40.0);
        // 標題列：左邊標題與資料夾，右邊「錄影｜截圖」與關閉。
        // 先排右邊，路徑只用剩下的寬度（太長時從中間省略），不會被「錄影｜截圖」蓋住
        let head = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), 48.0));
        let right_w = {
            let rui = &mut ui.new_child(UiBuilder::new().max_rect(head).layout(Layout::right_to_left(Align::Min)));
            if Btn::icon_only(Icon::Close).ghost().tooltip(tr!("關閉（Esc）", "Close (Esc)")).show(rui).clicked() {
                close = true;
            }
            rui.add_space(8.0);
            // 由右往左排：畫面上是「錄影｜截圖」
            if theme::segmented(rui, &mut d.kind, &[(Kind::Shot, tr!("截圖", "Screenshots")), (Kind::Video, tr!("錄影", "Recordings"))], true) {
                d.filter = LibraryFilter::All;
                if d.sort == LibrarySort::Duration {
                    d.sort = LibrarySort::New;
                }
                d.page = 1;
                d.selected.clear();
                d.data = None;
                d.dirty = true;
            }
            rui.min_rect().width()
        };
        {
            let left = Rect::from_min_max(head.min, pos2(head.max.x - right_w - 16.0, head.max.y));
            let lui = &mut ui.new_child(UiBuilder::new().max_rect(left).layout(Layout::top_down(Align::Min)));
            lui.spacing_mut().item_spacing.y = 2.0;
            let shot = d.kind == Kind::Shot;
            lui.add(egui::Label::new(RichText::new(if shot { tr!("全部截圖", "All screenshots") } else { tr!("全部錄影", "All recordings") }).font(theme::font_bold(17.0))).truncate());
            lui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let other = d.dir.is_some();
                // 留給下拉按鈕與「之前用過的資料夾」標籤
                let max_w = left.width() - 34.0 - if other { 120.0 } else { 0.0 };
                if folder_link(ui, &dir, max_w).clicked() {
                    req = Some(Req::OpenDir(dir.clone()));
                }
                let b = Btn::icon_only(Icon::ChevD).ghost().small().tooltip(tr!("看其他資料夾、變更儲存位置", "View another folder or change the save location")).show(ui);
                egui::Popup::menu(&b).show(|ui| {
                    ui.set_min_width(280.0);
                    let font = theme::font(13.0);
                    ui.label(theme::muted(ui, tr!("目前儲存位置", "Current save location")));
                    if ui.selectable_label(!other, middle_truncate(ui, &save_dir, &font, 360.0)).on_hover_text(&save_dir).clicked() {
                        req = Some(Req::Browse(None));
                    }
                    if !recent.is_empty() {
                        ui.add_space(4.0);
                        ui.label(theme::muted(ui, tr!("之前用過的資料夾", "Previously used folders")));
                        for r in &recent {
                            if ui.selectable_label(d.dir.as_deref() == Some(r.as_str()), middle_truncate(ui, r, &font, 360.0)).on_hover_text(r).clicked() {
                                req = Some(Req::Browse(Some(r.clone())));
                            }
                        }
                    }
                    ui.separator();
                    if ui.button(tr!("變更儲存位置…", "Change save location…")).clicked() {
                        req = Some(Req::ChooseDir);
                    }
                });
                if other {
                    ui.add_space(6.0);
                    chip(ui, tr!("之前用過的資料夾", "Previous folder"), Tone::Warn, false).on_hover_text(tr!("新的錄影與截圖不是存在這裡", "New recordings and screenshots aren't saved here"));
                }
            });
        }
        ui.advance_cursor_after_rect(head);
        let shot = d.kind == Kind::Shot;
        ui.add_space(8.0);
        // 搜尋、篩選、排序
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut d.query)
                    .hint_text(tr!("搜尋檔名、日期或字幕裡說過的話", "Search names, dates or words spoken (subtitles)"))
                    .desired_width(320.0)
                    .min_size(vec2(0.0, 30.0))
                    .vertical_align(Align::Center),
            );
            if d.focus_search {
                r.request_focus();
                d.focus_search = false;
            }
            if r.changed() {
                d.search_at = Some(Instant::now());
            }
            if !shot {
                let filters = [
                    (LibraryFilter::All, tr!("全部", "All")),
                    (LibraryFilter::Original, tr!("原始錄影", "Originals")),
                    (LibraryFilter::Cut, tr!("剪輯版", "Edited")),
                    (LibraryFilter::Speed, tr!("有加速版", "Has sped-up version")),
                    (LibraryFilter::Audio, tr!("有聲音", "Has audio")),
                ];
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
            let sorts = [
                (LibrarySort::New, tr!("最新的在前", "Newest first")),
                (LibrarySort::Old, tr!("最舊的在前", "Oldest first")),
                (LibrarySort::Duration, tr!("長度（長到短）", "Longest first")),
                (LibrarySort::Size, tr!("檔案大小（大到小）", "Largest first")),
            ];
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
            shot_grid(app, ui, &mut d, table, &dir, &mut req);
        } else {
            video_table(app, ui, &mut d, table, &dir, &mut req);
        }
        // 下方：選取與分頁
        let painter = ui.painter().clone();
        let foot = Rect::from_min_max(pos2(table.min.x, ui.max_rect().max.y - foot_h), ui.max_rect().max);
        painter.hline(foot.x_range(), foot.min.y - 4.0, Stroke::new(1.0, p.border));
        let fui = &mut ui.new_child(UiBuilder::new().max_rect(foot).layout(Layout::left_to_right(Align::Center)));
        let n = d.selected.len();
        let total = d.data.as_ref().map(|x| x.total).unwrap_or(0);
        // 沒有任何檔案時不顯示操作說明
        let hint = match (n, shot) {
            (0, _) if total == 0 => String::new(),
            (0, true) => tr!("可勾選多張一次刪除，或拼成一張", "Select several to delete them at once or combine them into one").into(),
            (0, false) => tr!("可勾選多筆一次刪除", "Select several to delete them at once").into(),
            _ if is_en() => format!("{n} file{} selected", if n == 1 { "" } else { "s" }),
            _ => format!("已選取 {n} 個檔案"),
        };
        fui.label(theme::muted(fui, hint));
        if n > 0 && Btn::new(tr!("移到資源回收筒", "Move to Recycle Bin")).icon(Icon::Trash).danger().small().show(fui).clicked() {
            req = Some(Req::DeleteSelected);
        }
        let mp4s: Vec<String> = d.selected.iter().filter(|p| p.to_lowercase().ends_with(".mp4")).cloned().collect();
        if !shot
            && mp4s.len() >= 2
            && Btn::new(tr!("合併成一支", "Merge into one"))
                .small()
                .tooltip(tr!(
                    "把勾選的錄影依時間順序接成一支（大小以最早的那支為準）；GIF 不算",
                    "Joins the selected recordings in time order into one video (sized like the earliest one). GIFs are skipped"
                ))
                .show(fui)
                .clicked()
        {
            req = Some(Req::Merge(mp4s));
        }
        if shot && n >= 2 {
            let b = Btn::new(tr!("拼成一張", "Combine into one"))
                .small()
                .tooltip(tr!("把勾選的截圖依時間順序拼成一張新的截圖", "Combines the selected screenshots in time order into a new screenshot"))
                .show(fui);
            egui::Popup::menu(&b).show(|ui| {
                ui.set_min_width(150.0);
                if ui.button(tr!("左右排", "Side by side")).clicked() {
                    req = Some(Req::Combine(false));
                }
                if ui.button(tr!("上下排", "Top to bottom")).clicked() {
                    req = Some(Req::Combine(true));
                }
            });
        }
        fui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let pages = d.data.as_ref().map(|x| x.pages).unwrap_or(1);
            if total == 0 {
                return;
            }
            if Btn::icon_only(Icon::ChevR).ghost().small().enabled(d.page < pages).tooltip(tr!("下一頁", "Next page")).show(ui).clicked() {
                d.page += 1;
                d.dirty = true;
            }
            if let Some(x) = d.data.as_ref().filter(|x| x.total > 0) {
                let text = if is_en() {
                    format!("Page {} of {}・{} item{}", d.page, x.pages.max(1), x.total, if x.total == 1 { "" } else { "s" })
                } else {
                    format!("第 {} / {} 頁・共 {} 筆", d.page, x.pages.max(1), x.total)
                };
                ui.label(RichText::new(text).font(theme::font(12.5)));
            }
            if Btn::icon_only(Icon::ChevL).ghost().small().enabled(d.page > 1).tooltip(tr!("上一頁", "Previous page")).show(ui).clicked() {
                d.page -= 1;
                d.dirty = true;
            }
        });
    });
    if modal.should_close() && app.ask.is_none() {
        close = true;
    }
    if close || matches!(req, Some(Req::Record)) {
        if matches!(req, Some(Req::Record)) {
            start_recording(app);
        }
        return;
    }
    let dirty = d.dirty;
    let jump = d.jump.take();
    app.library = Some(d);
    if dirty {
        load(app);
    }
    if let Some((e, t)) = jump {
        super::viewer::open_at(app, e, t);
    }
    match req {
        None | Some(Req::Record) => {}
        Some(Req::Entry(act, e)) => {
            // 剪輯影片、製作加速版時關掉清單；編輯截圖蓋在清單上，關掉編輯就回到清單
            if matches!(act, EntryAction::Edit | EntryAction::Export | EntryAction::Compress | EntryAction::Vertical) && !super::dialogs::is_image(&e.media.path) {
                app.library = None;
            }
            app.act(act, e);
        }
        Some(Req::Rename(e)) => rename_entry(app, e),
        Some(Req::DeleteSelected) => remove_selected(app),
        Some(Req::ChooseDir) => super::settings_dialog::choose_output_dir(app),
        Some(Req::OpenDir(dir)) => {
            if let Err(e) = actions::open(actions::OpenAction::Folder, &dir) {
                app.toast(e.message().to_string(), true);
            }
        }
        Some(Req::Browse(dir)) => {
            if let Some(d) = &mut app.library {
                if d.dir != dir {
                    d.dir = dir;
                    d.page = 1;
                    d.selected.clear();
                    d.data = None;
                    d.dirty = true;
                }
            }
        }
        Some(Req::Merge(paths)) => {
            let core = app.core.clone();
            let n = paths.len();
            app.spawn(async move { actions::merge_start(&core, &paths, None).await }, move |app, r| match r {
                Ok(_) => {
                    if let Some(d) = &mut app.library {
                        d.selected.clear();
                    }
                    app.toast(trf!("開始合併 {n} 支錄影，完成後會出現在清單", "Merging {n} recordings. The result will appear in the list when done"), false);
                }
                Err(e) => app.toast(e.message().to_string(), true),
            });
        }
        Some(Req::Combine(vertical)) => {
            let paths: Vec<String> = app.library.as_ref().map(|d| d.selected.iter().cloned().collect()).unwrap_or_default();
            let core = app.core.clone();
            app.spawn(async move { core.combine_shots(paths, vertical).await }, |app, r| match r {
                Ok(shot) => {
                    if let Some(d) = &mut app.library {
                        d.selected.clear();
                    }
                    // 狀態更新的「已截圖」不用再顯示一次
                    app.last_shot_seq = app.last_shot_seq.max(shot.seq);
                    let copied = if shot.copied { tr!("，並複製到剪貼簿", " and copied to the clipboard") } else { "" };
                    app.toast(trf!("已拼成 {}（{}×{}）{copied}", "Combined into {} ({}×{}){copied}", super::dialogs::file_name(&shot.path), shot.width, shot.height), false);
                    app.shots_changed();
                }
                Err(e) => app.toast(e.message().to_string(), true),
            });
        }
    }
}

/// 可以開始錄影（有 FFmpeg、沒在錄影或轉檔）
fn can_record(app: &UiApp) -> bool {
    app.env.ffmpeg.found && app.env.ffmpeg.encoder.is_some() && !app.exporting() && !app.locked()
}

/// 關掉清單、用目前的設定開始錄影（與主畫面的「開始錄影」相同）
fn start_recording(app: &mut UiApp) {
    app.library = None;
    let config = app.s.record_config(&app.env);
    let core = app.core.clone();
    app.guarded(async move { actions::record_start(&core, config).await }, |_, _| {});
}

/// 錄影：表格（高度固定，伺服器依每筆的子列數分頁，一頁剛好放滿）。
/// 依日期分組：每天第一筆在「日期」欄寫「今天」「昨天」「10/08（三）」，其餘只寫時間
fn video_table(app: &mut UiApp, ui: &mut egui::Ui, d: &mut LibraryDialog, table: Rect, dir: &str, req: &mut Option<Req>) {
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
    let cols = [36.0, 104.0, 0.0, 84.0, 104.0, 90.0, 166.0];
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
    // 標題列（核取方塊與每一列的對齊）
    {
        let head = Rect::from_min_size(table.min, vec2(table.width(), head_h));
        let all = !rows.is_empty() && rows.iter().all(|e| d.selected.contains(&e.media.path) && e.exports.iter().all(|x| d.selected.contains(&x.media.path)));
        let mut all2 = all;
        let ui2 = &mut ui
            .new_child(UiBuilder::new().max_rect(Rect::from_min_max(head.min + vec2(10.0, 0.0), pos2(head.min.x + col_x(1) - table.min.x, head.max.y))).layout(Layout::left_to_right(Align::Center)));
        if theme::check(ui2, &mut all2, "").on_hover_text(tr!("全選本頁", "Select all on this page")).changed() {
            for e in &rows {
                d.toggle(&e.media.path, all2);
            }
        }
        let heads = [(tr!("日期", "Date"), false), (tr!("檔案", "File"), false), (tr!("長度", "Duration"), true), (tr!("解析度", "Resolution"), true), (tr!("大小", "Size"), true)];
        for (i, (t, right)) in heads.iter().enumerate() {
            let x0 = col_x(i + 1);
            let x1 = col_x(i + 2);
            let (pos, align) = if *right { (pos2(x1 - 10.0, head.center().y), egui::Align2::RIGHT_CENTER) } else { (pos2(x0 + 6.0, head.center().y), egui::Align2::LEFT_CENTER) };
            painter.text(pos, align, *t, theme::font_bold(12.5), p.muted);
        }
    }
    // 內容
    let mut y = table.min.y + head_h + 2.0;
    let times = time_labels(rows.iter().map(|e| (e.media.name.as_str(), e.media.time_ms())));
    let today = chrono::Local::now().date_naive();
    let mut last_day = None;
    if rows.is_empty() {
        let can = can_record(app);
        empty_list(ui, d, table, dir, req, tr!("這個資料夾還沒有錄影。", "No recordings in this folder yet."), tr!("沒有符合條件的錄影。", "No matching recordings."), can);
    }
    for e in &rows {
        let row = Rect::from_min_size(pos2(table.min.x, y), vec2(table.width(), LIBRARY_ROW_MAIN_PX as f32));
        let resp = ui.interact(row, Id::new(("librow", &e.media.path)), Sense::hover());
        // 子列（加速版）也算在這一列的範圍內：滑鼠在子列上時，原片的按鈕也亮起來
        let sub_rows = e.exports.len() + e.hits.len().min(LIBRARY_HITS_SHOWN);
        let group = Rect::from_min_size(row.min, vec2(row.width(), LIBRARY_ROW_MAIN_PX as f32 + sub_rows as f32 * LIBRARY_ROW_SUB_PX as f32));
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
        if theme::check(rui, &mut on, "").changed() {
            d.toggle(&e.media.path, on);
        }
        if new_day {
            if let Some(dd) = day {
                painter.text(pos2(col_x(1) + 6.0, row.center().y), egui::Align2::LEFT_CENTER, day_label(dd, today), theme::font_bold(13.0), p.text);
            }
        }
        // 檔案欄：縮圖、時間、檔名（資料夾裡看到的名稱），右邊是剪輯版、有聲音
        let name_rect = Rect::from_min_max(pos2(col_x(2), row.min.y), pos2(col_x(3), row.max.y));
        let thumb = Rect::from_min_size(pos2(name_rect.min.x + 4.0, row.center().y - 15.0), vec2(53.0, 30.0));
        painter.rect_filled(thumb, CornerRadius::same(4), p.surface2);
        if let Some(t) = app.thumb(&e.media.path, e.media.mtime) {
            theme::paint_thumb(&painter, thumb, &t);
        }
        {
            let cell = Rect::from_min_max(pos2(thumb.max.x + 10.0, row.min.y), pos2(name_rect.max.x - 8.0, name_rect.max.y));
            // 先排右邊的標記，檔名用剩下的寬度（太長時截斷，不會擠掉標記）
            let tui = &mut ui.new_child(UiBuilder::new().max_rect(cell).layout(Layout::right_to_left(Align::Center)));
            if e.media.has_audio == Some(true) {
                audio_mark(tui);
            }
            if is_cut_name(&e.media.name) {
                chip(tui, tr!("剪輯版", "Edited"), Tone::Warn, false);
            }
            let tags_w = tui.min_rect().width();
            let left = Rect::from_min_max(cell.min, pos2(cell.max.x - tags_w - 6.0, cell.max.y));
            let nui = &mut ui.new_child(UiBuilder::new().max_rect(left).layout(Layout::left_to_right(Align::Center)));
            nui.label(RichText::new(time.unwrap_or_default()).font(theme::font_bold(13.0)));
            // 自己取的名稱用一般字色，預設檔名（Rec_日期_時間）用灰色
            let custom = !is_default_name(&e.media.name);
            let name = RichText::new(&e.media.name).font(theme::font(if custom { 13.0 } else { 12.5 })).color(if custom { p.text } else { p.muted });
            let r = nui.add(egui::Label::new(name).truncate().sense(Sense::click())).on_hover_text(trf!("{}\n按兩下改名", "{}\nDouble-click to rename", e.media.name));
            if r.double_clicked() {
                *req = Some(Req::Rename(e.clone()));
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
            for (act, icon, tip) in [
                (EntryAction::Play, Icon::Play, tr!("播放", "Play")),
                (EntryAction::Reveal, Icon::Folder, tr!("在資料夾中顯示", "Show in folder")),
                (EntryAction::Edit, Icon::Cut, tr!("剪輯", "Edit")),
            ] {
                if Btn::icon_only(icon).ghost().small().quiet(!hot).tooltip(tip).show(aui).clicked() {
                    *req = Some(Req::Entry(act, e.clone()));
                }
            }
            let tip = if e.exports.is_empty() { tr!("重新命名", "Rename") } else { tr!("重新命名（加速版、GIF 一起改）", "Rename (sped-up versions and GIFs too)") };
            if Btn::icon_only(Icon::Rename).ghost().small().quiet(!hot).tooltip(tip).show(aui).clicked() {
                *req = Some(Req::Rename(e.clone()));
            }
            // 不常用的收進「⋯」
            let more = Btn::icon_only(Icon::More).ghost().small().quiet(!hot).tooltip(tr!("更多", "More")).show(aui);
            egui::Popup::menu(&more).show(|ui| {
                ui.set_min_width(190.0);
                if ui.button(tr!("製作加速版 / GIF", "Make sped-up video / GIF")).clicked() {
                    *req = Some(Req::Entry(EntryAction::Export, e.clone()));
                }
                if ui.button(tr!("壓縮（傳 LINE / Email）", "Compress (for LINE / email)")).clicked() {
                    *req = Some(Req::Entry(EntryAction::Compress, e.clone()));
                }
                if ui.button(tr!("做成直式影片（Shorts / Reels）…", "Make a vertical video (Shorts / Reels)…")).clicked() {
                    *req = Some(Req::Entry(EntryAction::Vertical, e.clone()));
                }
                if ui.button(tr!("複製檔案（貼到 LINE、資料夾）", "Copy file (paste into LINE or a folder)")).clicked() {
                    *req = Some(Req::Entry(EntryAction::CopyFile, e.clone()));
                }
                if e.media.has_audio != Some(false) && ui.button(tr!("存成 M4A（只留聲音）", "Save as M4A (audio only)")).clicked() {
                    *req = Some(Req::Entry(EntryAction::SaveAudio, e.clone()));
                }
                if !e.media.chapters.is_empty() && ui.button(tr!("複製章節（貼到 YouTube）", "Copy chapters (for YouTube)")).clicked() {
                    *req = Some(Req::Entry(EntryAction::CopyChapters, e.clone()));
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
            if theme::check(sui, &mut on, "").changed() {
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
                let nui = &mut ui.new_child(UiBuilder::new().max_rect(Rect::from_min_max(pos2(bx + 18.0, row.min.y), pos2(col_x(3) - 8.0, row.max.y))).layout(Layout::left_to_right(Align::Center)));
                chip(nui, &export_label(x), Tone::Accent, false).on_hover_text(trf!("{}（{}×）", "{} ({}×)", x.media.name, speed_label(x.speed)));
                nui.add(egui::Label::new(RichText::new(&x.media.name).font(theme::font(12.0)).color(p.muted)).truncate());
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
            for (act, icon, tip) in [(EntryAction::Play, Icon::Play, tr!("播放", "Play")), (EntryAction::Reveal, Icon::Folder, tr!("在資料夾中顯示", "Show in folder"))] {
                if Btn::icon_only(icon).ghost().small().quiet(!hot).tooltip(tip).show(aui).clicked() {
                    *req = Some(Req::Entry(act, LibraryEntry { media: x.media.clone(), exports: vec![], hits: vec![] }));
                }
            }
            y += LIBRARY_ROW_SUB_PX as f32;
        }
        // 搜尋到的說過的話：時間與那一句（點一下從那裡播放）
        let shown = e.hits.len().min(LIBRARY_HITS_SHOWN);
        for (i, h) in e.hits.iter().take(shown).enumerate() {
            let row = Rect::from_min_size(pos2(table.min.x, y), vec2(table.width(), LIBRARY_ROW_SUB_PX as f32));
            let resp = ui.interact(row, Id::new(("libhit", &e.media.path, i)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
            if resp.hovered() {
                painter.rect_filled(row, CornerRadius::same(4), p.accent.gamma_multiply(0.08));
            }
            painter.hline(row.x_range(), row.max.y, Stroke::new(1.0, p.border.gamma_multiply(0.6)));
            let bx = col_x(2) + 30.0;
            painter.line_segment([pos2(bx, row.min.y - 6.0), pos2(bx, row.center().y)], Stroke::new(1.0, p.border_strong));
            painter.line_segment([pos2(bx, row.center().y), pos2(bx + 12.0, row.center().y)], Stroke::new(1.0, p.border_strong));
            let more = if i + 1 == shown && e.hits.len() > shown { trf!("　（還有 {} 句）", " ({} more)", e.hits.len() - shown) } else { String::new() };
            let mut job = egui::text::LayoutJob::default();
            job.append(&format!("{}  ", video_clock(h.start)), 0.0, egui::TextFormat::simple(theme::mono(12.5), p.accent));
            highlight(&mut job, &h.text, d.query.trim(), p.text, p.accent);
            job.append(&more, 0.0, egui::TextFormat::simple(theme::font(12.5), p.muted));
            job.wrap = egui::text::TextWrapping::truncate_at_width(col_x(6) - bx - 30.0);
            let galley = ui.fonts_mut(|f| f.layout_job(job));
            painter.galley(pos2(bx + 18.0, row.center().y - galley.size().y / 2.0), galley, p.text);
            if resp.clicked() {
                d.jump = Some((e.clone(), h.start));
            }
            y += LIBRARY_ROW_SUB_PX as f32;
        }
    }
}

/// 清單是空的：讀取中、資料夾讀不到（選別的資料夾）、還沒有檔案（開始錄影）、搜尋沒有結果（清除搜尋）
#[allow(clippy::too_many_arguments)]
fn empty_list(ui: &mut egui::Ui, d: &mut LibraryDialog, area: Rect, dir: &str, req: &mut Option<Req>, none: &str, no_match: &str, record: bool) {
    let open_dir = tr!("開啟資料夾", "Open folder");
    if d.loading {
        empty_state(ui, area, tr!("讀取中…", "Loading…"), &[]);
    } else if let Some(e) = d.error.clone() {
        // 看之前用過的資料夾時，可以回到目前的儲存位置
        let mut buttons = vec![(tr!("選擇資料夾…", "Choose folder…"), true), (open_dir, false)];
        if d.dir.is_some() {
            buttons.push((tr!("看目前的儲存位置", "View current save location"), false));
        }
        match empty_state(ui, area, &e, &buttons) {
            Some(0) => *req = Some(Req::ChooseDir),
            Some(1) => *req = Some(Req::OpenDir(dir.to_string())),
            Some(_) => *req = Some(Req::Browse(None)),
            None => {}
        }
    } else if d.data.as_ref().is_some_and(|x| x.total == 0) && d.query.trim().is_empty() && d.filter == LibraryFilter::All {
        let mut buttons = vec![(open_dir, false), (tr!("變更儲存位置…", "Change save location…"), false)];
        if record {
            buttons.insert(0, (tr!("開始錄影", "Start recording"), true));
        }
        match empty_state(ui, area, none, &buttons).map(|i| if record { i } else { i + 1 }) {
            Some(0) => *req = Some(Req::Record),
            Some(1) => *req = Some(Req::OpenDir(dir.to_string())),
            Some(_) => *req = Some(Req::ChooseDir),
            None => {}
        }
    } else if d.data.is_some() {
        let searching = !d.query.trim().is_empty();
        let buttons = if searching { vec![(tr!("清除搜尋", "Clear search"), false)] } else { vec![] };
        if empty_state(ui, area, no_match, &buttons).is_some() {
            d.query.clear();
            d.page = 1;
            d.dirty = true;
        }
    }
}

/// 一句話，搜尋的字用強調色、粗體
fn highlight(job: &mut egui::text::LayoutJob, text: &str, query: &str, color: Color32, accent: Color32) {
    let normal = egui::TextFormat::simple(theme::font(12.5), color);
    let strong = egui::TextFormat::simple(theme::font_bold(12.5), accent);
    let lower = text.to_lowercase();
    let q = query.to_lowercase();
    // 小寫後長度不變時才標示（中文、英文都是）；否則整句一般顯示
    if q.is_empty() || lower.len() != text.len() {
        job.append(text, 0.0, normal);
        return;
    }
    let mut at = 0;
    while let Some(i) = lower[at..].find(&q).map(|i| i + at) {
        job.append(&text[at..i], 0.0, normal.clone());
        job.append(&text[i..i + q.len()], 0.0, strong.clone());
        at = i + q.len();
    }
    job.append(&text[at..], 0.0, normal);
}

/// 「有聲音」：灰色小喇叭（滑鼠提示說明），不用彩色標籤
pub fn audio_mark(ui: &mut egui::Ui) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
    theme::paint_icon(ui.painter(), r, Icon::Speaker, theme::pal(ui).muted);
    resp.on_hover_text(tr!("有聲音", "Has audio"))
}

/// 截圖：縮圖格（一頁放幾張依對話框大小），點縮圖開啟
fn shot_grid(app: &mut UiApp, ui: &mut egui::Ui, d: &mut LibraryDialog, area: Rect, dir: &str, req: &mut Option<Req>) {
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
        let none = match app.keys.label(2) {
            k if k.is_empty() => tr!("還沒有截圖。按主畫面的「截圖」試試看。", "No screenshots yet. Try “Screenshot” in the main window.").to_string(),
            k => trf!("還沒有截圖。按主畫面的「截圖」或 {k} 試試看。", "No screenshots yet. Try “Screenshot” in the main window or press {k}."),
        };
        empty_list(ui, d, area, dir, req, &none, tr!("沒有符合條件的截圖。", "No matching screenshots."), false);
    }
    let dates = date_labels(items.iter().map(|e| (e.media.name.as_str(), e.media.time_ms())));
    for (i, e) in items.iter().enumerate() {
        let (c, r) = ((i % cols as usize) as f32, (i / cols as usize) as f32);
        let cell = Rect::from_min_size(pos2(area.min.x + c * (cell_w + gap), area.min.y + r * (cell_h + gap)), vec2(cell_w, cell_h));
        let hot = ui.rect_contains_pointer(cell);
        let on = d.selected.contains(&e.media.path);
        let img = Rect::from_min_size(cell.min, vec2(cell_w, img_h));
        let resp = ui.interact(img, Id::new(("shotcell", &e.media.path)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(trf!(
            "{}\n點一下開啟",
            "{}\nClick to open",
            e.media.name
        ));
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
            *req = Some(Req::Entry(EntryAction::Play, e.clone()));
        }
        // 左上角勾選（白底，在深色縮圖上也看得到）
        let cb = Rect::from_min_size(img.min + vec2(6.0, 6.0), vec2(24.0, 22.0));
        if on || resp.hovered() || ui.rect_contains_pointer(cb) {
            painter.rect_filled(cb, CornerRadius::same(5), p.surface.gamma_multiply(0.92));
        }
        {
            let cui = &mut ui.new_child(UiBuilder::new().max_rect(cb.shrink2(vec2(4.0, 1.0))));
            let mut v = on;
            if theme::check(cui, &mut v, "").changed() {
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
            if Btn::icon_only(Icon::Rename).ghost().small().quiet(!hot).tooltip(tr!("重新命名", "Rename")).show(ui).clicked() {
                *req = Some(Req::Rename(e.clone()));
            }
            if Btn::icon_only(Icon::Folder).ghost().small().quiet(!hot).tooltip(tr!("在資料夾中顯示", "Show in folder")).show(ui).clicked() {
                *req = Some(Req::Entry(EntryAction::Reveal, e.clone()));
            }
            if Btn::icon_only(Icon::Edit)
                .ghost()
                .small()
                .quiet(!hot)
                .tooltip(tr!("編輯：標註、遮個資、裁切（另存一張）", "Edit: annotate, redact personal info, crop (saves a copy)"))
                .show(ui)
                .clicked()
            {
                *req = Some(Req::Entry(EntryAction::Edit, e.clone()));
            }
            if Btn::icon_only(Icon::Eye).ghost().small().quiet(!hot).tooltip(tr!("檢視", "View")).show(ui).clicked() {
                *req = Some(Req::Entry(EntryAction::Play, e.clone()));
            }
        });
    }
}

/// 改名：原片與底下的加速版一起改；送出時檢查衝突、使用中，錯誤留在對話框裡
pub fn rename_entry(app: &mut UiApp, entry: LibraryEntry) {
    let old = base_name(&entry.media.name);
    let n = entry.exports.len();
    let mut ask = Ask::input(tr!("重新命名", "Rename"), tr!("新名稱", "New name"), old.clone(), tr!("改名", "Rename"), move |app, value| {
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
                app.toast(trf!("已改名為 {name}", "Renamed to {name}"), false);
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
        ask.message = Some(if is_en() {
            if n == 1 {
                "Its sped-up version will be renamed too.".to_string()
            } else {
                format!("Its {n} sped-up versions will be renamed too.")
            }
        } else {
            format!("底下的 {n} 個加速版會一起改名。")
        });
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
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let (title, message) = if is_en() {
        let kept_text = match kept {
            0 => String::new(),
            1 => "\nThe unselected sped-up version will be kept.".to_string(),
            k => format!("\nThe {k} unselected sped-up versions will be kept."),
        };
        let them = if list.len() == 1 { "it" } else { "them" };
        (format!("Move {} file{} to the Recycle Bin?", list.len(), plural(list.len())), format!("You can restore {them} from the Recycle Bin.{kept_text}"))
    } else {
        (format!("把 {} 個檔案移到資源回收筒？", list.len()), format!("可從資源回收筒還原。{}", if kept > 0 { format!("\n未勾選的 {kept} 個加速版會保留。") } else { String::new() }))
    };
    let mut ask = Ask::confirm(title, message, tr!("移到資源回收筒", "Move to Recycle Bin"), move |app, _| {
        app.ask = None;
        match actions::delete(&app.core, &list) {
            Ok(n) => {
                let text = if is_en() { format!("Moved {n} file{} to the Recycle Bin", plural(n)) } else { format!("已將 {n} 個檔案移到資源回收筒") };
                app.toast(text, false);
                if let Some(d) = &mut app.library {
                    d.selected.clear();
                    d.dirty = true;
                }
                app.load_recent();
            }
            Err(e) => app.toast(e.message().to_string(), true),
        }
    });
    ask.danger = true;
    ask.list = d.selected.iter().map(|p| file_name(p)).collect();
    ask.list.sort();
    app.ask = Some(ask);
}
