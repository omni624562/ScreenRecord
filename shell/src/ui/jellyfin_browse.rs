//! 瀏覽 Jellyfin：列出伺服器上的媒體庫、資料夾、影片（家人用手機傳上去的也看得到），點了就在檢視器裡串流播放。
//! 不在家時只要連得到 Jellyfin 也能看。見 core::jellyfin 的 views、items、stream_url

use super::theme::{self, Btn, Icon};
use super::{viewer, UiApp};
use eframe::egui::{self, pos2, vec2, Align, Align2, Id, Layout, Rect, RichText, Sense};
use screenrecorder_core::jellyfin::{JfItem, JfKind};
use screenrecorder_core::types::{LibraryEntry, MediaInfo};
use screenrecorder_core::{tr, trf};
use std::collections::HashMap;

/// 一頁幾個
const PAGE: usize = 60;
/// 每格的寬度（縮圖 16:9）
const CELL_W: f32 = 196.0;

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
    total: usize,
    page: usize,
    error: Option<String>,
    seq: u64,
    posters: HashMap<String, Poster>,
}

/// 打開瀏覽視窗（沒設定 Jellyfin 時提示到設定填）
pub fn open(app: &mut UiApp) {
    if app.jellyfin().is_none() {
        return app.toast(tr!("要先在「設定 → 存檔與分享」填 Jellyfin 的網址和 API 金鑰", "Fill in the Jellyfin address and API key in Settings → Saving & sharing first"), true);
    }
    app.jf_browse = Some(JfBrowse::default());
    load(app);
}

/// 讀目前這一層（或搜尋結果）的這一頁
fn load(app: &mut UiApp) {
    let Some(jf) = app.jellyfin() else { return };
    let preferred = app.s.jellyfin_user.clone();
    let Some(d) = app.jf_browse.as_mut() else { return };
    d.seq += 1;
    d.items = None;
    d.error = None;
    d.searched = d.search.trim().to_string();
    let (seq, user, parent, search, start) = (d.seq, d.user.clone(), d.path.last().map(|(id, _)| id.clone()), d.searched.clone(), d.page * PAGE);
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || -> Result<(String, Vec<JfItem>, usize), String> {
                let user = match user {
                    Some(u) => u,
                    None => jf.browse_user(&preferred)?,
                };
                if parent.is_none() && search.is_empty() {
                    let views = jf.views(&user)?;
                    let n = views.len();
                    return Ok((user, views, n));
                }
                let (items, total) = jf.items(&user, parent.as_deref(), &search, start, PAGE)?;
                Ok((user, items, total))
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        },
        move |app, r| {
            let Some(d) = app.jf_browse.as_mut().filter(|d| d.seq == seq) else { return };
            match r {
                Ok((user, items, total)) => {
                    d.user = Some(user);
                    d.items = Some(items);
                    d.total = total;
                }
                Err(e) => d.error = Some(e),
            }
        },
    );
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

/// 在檢視器裡串流播放
fn play(app: &mut UiApp, item: &JfItem) {
    let Some(jf) = app.jellyfin() else { return };
    if item.duration <= 0.0 {
        return app.toast(tr!("Jellyfin 還沒分析好這支影片（長度不明），等一下再試", "Jellyfin hasn't finished analyzing this video yet. Try again shortly"), true);
    }
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
        ..Default::default()
    };
    viewer::open(app, LibraryEntry { media, exports: vec![], hits: vec![] });
    viewer::set_autoplay(app, true);
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

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut d) = app.jf_browse.take() else { return };
    let mut close = false;
    let mut reload = false;
    let mut open_item: Option<JfItem> = None;
    let mut want_posters: Vec<String> = vec![];
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
        // 目前位置：媒體庫 › family › 2024，點前面的回到那一層
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(2.0, 4.0);
            if !d.searched.is_empty() {
                ui.label(RichText::new(trf!("搜尋「{}」", "Search “{}”", d.searched)).color(p.muted));
                return;
            }
            if Btn::new(tr!("媒體庫", "Libraries")).icon(Icon::Folder).small().ghost().selected(d.path.is_empty()).show(ui).clicked() && !d.path.is_empty() {
                d.path.clear();
                d.page = 0;
                reload = true;
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
        });
        ui.add_space(6.0);
        let body_h = h - 110.0;
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
                    ui.label(RichText::new(tr!("這裡沒有影片", "No videos here")).color(p.muted));
                });
            }
            (Some(items), None) => {
                egui::ScrollArea::vertical().max_height(body_h).min_scrolled_height(body_h).auto_shrink([false, false]).show(ui, |ui| {
                    let gap = 12.0;
                    let cols = ((ui.available_width() + gap) / (CELL_W + gap)).floor().max(1.0) as usize;
                    let cell_w = (ui.available_width() - gap * (cols as f32 - 1.0)) / cols as f32;
                    let img_h = (cell_w * 9.0 / 16.0).round();
                    let cell_h = img_h + 46.0;
                    let clip = ui.clip_rect();
                    for row in items.chunks(cols) {
                        let (row_rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), cell_h), Sense::hover());
                        for (i, it) in row.iter().enumerate() {
                            let cell = Rect::from_min_size(pos2(row_rect.min.x + i as f32 * (cell_w + gap), row_rect.min.y), vec2(cell_w, cell_h));
                            let resp = ui.interact(cell, Id::new(("jf-cell", &it.id)), Sense::click());
                            let img = Rect::from_min_size(cell.min, vec2(cell_w, img_h));
                            let painter = ui.painter();
                            painter.rect_filled(img, 8.0, p.surface2);
                            match d.posters.get(&it.id) {
                                Some(Poster::Ready(tex)) => theme::paint_thumb_cover(painter, img, tex),
                                None if it.has_image && clip.intersects(cell) => want_posters.push(it.id.clone()),
                                _ => {}
                            }
                            let no_img = !matches!(d.posters.get(&it.id), Some(Poster::Ready(_)));
                            if no_img {
                                let icon = if it.kind == JfKind::Folder { Icon::Folder } else { Icon::Video };
                                theme::paint_icon(painter, Rect::from_center_size(img.center(), vec2(32.0, 32.0)), icon, p.muted);
                            }
                            if resp.hovered() {
                                painter.rect_stroke(img, 8.0, egui::Stroke::new(2.0, p.accent), egui::StrokeKind::Inside);
                                if it.kind == JfKind::Video {
                                    theme::paint_icon(painter, Rect::from_center_size(img.center(), vec2(40.0, 40.0)), Icon::Play, egui::Color32::WHITE);
                                }
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
                                JfKind::Video => {
                                    [it.date.clone(), if it.duration > 0.0 { clock(it.duration) } else { String::new() }].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")
                                }
                            };
                            painter.text(pos2(cell.min.x, img.max.y + 24.0), Align2::LEFT_TOP, sub, theme::font(11.5), p.muted);
                            let resp = resp.on_hover_text(&it.name);
                            if resp.clicked() {
                                open_item = Some(it.clone());
                            }
                        }
                        ui.add_space(gap);
                    }
                });
            }
        }
        // 換頁
        let pages = d.total.div_ceil(PAGE).max(1);
        if (!d.path.is_empty() || !d.searched.is_empty()) && pages > 1 {
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
    // 檢視器開著時 Esc 是關檢視器，不要連瀏覽視窗一起關
    if close || (modal.should_close() && app.viewer.is_none()) {
        return;
    }
    if let Some(it) = open_item {
        match it.kind {
            JfKind::Folder => {
                d.path.push((it.id.clone(), it.name.clone()));
                d.search.clear();
                d.page = 0;
                reload = true;
            }
            JfKind::Video => {
                app.jf_browse = Some(d);
                play(app, &it);
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
    for id in want_posters {
        load_poster(app, &id);
    }
}
