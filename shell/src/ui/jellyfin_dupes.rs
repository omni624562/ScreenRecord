//! 找出 Jellyfin 上重複的影片（從檔案庫的 Jellyfin 分頁開啟）：一組一組並排列出來，預設留畫質最好的那一份、其他打勾，
//! 確認後從 Jellyfin 刪除打勾的。比對方法見 core::jf_dupes

use super::theme::{self, Btn, Icon};
use super::{viewer, UiApp};
use eframe::egui::{self, vec2, Align, Id, Layout, Rect, RichText, Sense};
use screenrecorder_core::format::{format_bytes, short_clock};
use screenrecorder_core::jf_dupes::{self, DupGroup, JfVideo, Progress};
use screenrecorder_core::types::{LibraryEntry, MediaInfo};
use screenrecorder_core::{tr, trf};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;

enum Phase {
    /// 讀清單、比對畫面中
    Scanning(Arc<Progress>),
    Done(Vec<DupGroup>),
    Error(String),
}

pub struct JfDupes {
    user: String,
    phase: Phase,
    /// 打勾要刪的（影片編號）
    marked: HashSet<String>,
    posters: HashMap<String, Option<egui::TextureHandle>>,
}

/// 開啟並開始找（user：Jellyfin 分頁用的使用者，看過了沒照這個使用者）
pub fn open(app: &mut UiApp, user: String) {
    let Some(jf) = app.jellyfin() else { return };
    let Some(ffmpeg) = app.core.ffmpeg_path() else {
        return app.toast(tr!("找不到 FFmpeg，沒辦法比對畫面", "FFmpeg not found, so the pictures can't be compared"), true);
    };
    let progress = Arc::new(Progress::default());
    app.jf_dupes = Some(JfDupes { user: user.clone(), phase: Phase::Scanning(progress.clone()), marked: HashSet::new(), posters: HashMap::new() });
    let ctx = app.ctx.clone();
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || {
                let videos = jf.all_videos(&user)?;
                ctx.request_repaint();
                Ok::<_, String>(jf_dupes::find(&ffmpeg, &videos, |id| jf.stream_url(id), &progress))
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        },
        |app, r| {
            let Some(d) = app.jf_dupes.as_mut() else { return };
            match r {
                Ok(Some(groups)) => {
                    // 預設留第一份（畫質最好），其他打勾
                    d.marked = groups.iter().flat_map(|g| g.items.iter().skip(1).map(|v| v.id.clone())).collect();
                    d.phase = Phase::Done(groups);
                }
                // 取消了
                Ok(None) => app.jf_dupes = None,
                Err(e) => d.phase = Phase::Error(e),
            }
        },
    );
}

/// 從 Jellyfin 刪掉了：清單移除它們，只剩一支的組也移除
pub fn removed(app: &mut UiApp, ids: &[String]) {
    let Some(d) = app.jf_dupes.as_mut() else { return };
    if let Phase::Done(groups) = &mut d.phase {
        for g in groups.iter_mut() {
            g.items.retain(|v| !ids.contains(&v.id));
        }
        groups.retain(|g| g.items.len() > 1);
    }
    d.marked.retain(|id| !ids.contains(id));
}

/// 在檢視器裡串流播放（比較看看是不是同一支）
fn play(app: &mut UiApp, user: &str, v: &JfVideo) {
    let Some(jf) = app.jellyfin() else { return };
    let file = v.path.rsplit(['/', '\\']).next().unwrap_or("").to_string();
    let ext = file.rsplit_once('.').map(|(_, e)| e.to_string()).unwrap_or_else(|| "mp4".into());
    let media = MediaInfo {
        path: jf.stream_url(&v.id),
        name: format!("{}.{ext}", v.name),
        duration_sec: Some(v.duration),
        width: (v.width > 0).then_some(v.width),
        height: (v.height > 0).then_some(v.height),
        ..Default::default()
    };
    viewer::open_remote(app, LibraryEntry { media, exports: vec![], hits: vec![] }, viewer::Remote::new(&v.id, user, vec![], &file), None);
}

/// 在伺服器上的資料夾（路徑去掉檔名）
fn folder(path: &str) -> &str {
    path.rsplit_once(['/', '\\']).map_or("", |(d, _)| d)
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut d) = app.jf_dupes.take() else { return };
    let mut close = false;
    let mut cancel = false;
    let mut delete = false;
    let mut play_it: Option<JfVideo> = None;
    let mut want_posters: Vec<String> = vec![];
    let modal = egui::Modal::new(Id::new("jellyfin-dupes")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        let p = theme::pal(ui);
        let w = (ctx.content_rect().width() - 100.0).clamp(420.0, 900.0);
        let h = (ctx.content_rect().height() - 160.0).clamp(300.0, 700.0);
        ui.set_width(w);
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr!("Jellyfin 上重複的影片", "Duplicate videos on Jellyfin")).font(theme::font_bold(17.0)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if Btn::icon_only(Icon::Close).ghost().small().tooltip(tr!("關閉（Esc）", "Close (Esc)")).show(ui).clicked() {
                    close = true;
                }
            });
        });
        ui.add_space(6.0);
        match &d.phase {
            Phase::Scanning(pr) => {
                let (done, total) = (pr.done.load(Ordering::Relaxed), pr.total.load(Ordering::Relaxed));
                ui.horizontal(|ui| {
                    ui.spinner();
                    let text = if total == 0 {
                        tr!("正在讀 Jellyfin 上的影片清單…", "Reading the list of videos on Jellyfin…").to_string()
                    } else {
                        trf!("正在比對畫面：{done} / {total} 支（長度差不多的才比）", "Comparing pictures: {done} / {total} (only videos of similar length)")
                    };
                    ui.label(text);
                });
                if total > 0 {
                    ui.add(egui::ProgressBar::new(done as f32 / total as f32).desired_height(6.0).fill(p.accent));
                }
                ui.label(
                    RichText::new(tr!(
                        "直接從 Jellyfin 讀每支影片裡幾個時間點的畫面來比，不用下載整支；比過的會記住，下次只比新的影片。",
                        "Pictures at a few points in each video are read straight from Jellyfin, without downloading whole videos. Results are remembered, so next time only new videos are compared."
                    ))
                    .font(theme::font(12.0))
                    .color(p.muted),
                );
                ui.add_space(8.0);
                if Btn::new(tr!("取消", "Cancel")).ghost().show(ui).clicked() {
                    cancel = true;
                }
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
            }
            Phase::Error(e) => {
                ui.label(RichText::new(trf!("讀不到 Jellyfin：{e}", "Couldn't reach Jellyfin: {e}")).color(p.warn));
            }
            Phase::Done(groups) if groups.is_empty() => {
                ui.label(RichText::new(tr!("沒有找到重複的影片 ✓", "No duplicate videos found ✓")).font(theme::font_bold(14.0)).color(p.ok));
                ui.label(
                    RichText::new(tr!("加速版、剪輯版長度不一樣，不會被當成重複。", "Sped-up and edited versions have different lengths, so they don't count as duplicates."))
                        .font(theme::font(12.0))
                        .color(p.muted),
                );
            }
            Phase::Done(groups) => {
                let saved: u64 = groups.iter().flat_map(|g| g.items.iter()).filter(|v| d.marked.contains(&v.id)).map(|v| v.size).sum();
                ui.label(
                    RichText::new(trf!(
                        "找到 {} 組。預設留畫質最好的那一份，其他打勾；按「播放」可以先看看是不是同一支。",
                        "Found {} groups. The best-quality copy is kept and the others are ticked; press Play to check they really are the same.",
                        groups.len()
                    ))
                    .font(theme::font(12.5))
                    .color(p.muted),
                );
                ui.add_space(6.0);
                egui::ScrollArea::vertical().max_height(h - 120.0).auto_shrink([false, true]).show(ui, |ui| {
                    for (gi, g) in groups.iter().enumerate() {
                        let title = if g.exact {
                            tr!("完全一樣的檔案", "Identical files")
                        } else {
                            tr!("內容一樣（檔案不同：畫質、壓縮方式不一樣）", "Same content (different files: quality or encoding differs)")
                        };
                        ui.add_space(4.0);
                        ui.label(RichText::new(format!("{}・{}", title, short_clock(g.items[0].duration))).font(theme::font_bold(13.5)));
                        for (i, v) in g.items.iter().enumerate() {
                            ui.push_id((gi, &v.id), |ui| {
                                ui.horizontal(|ui| {
                                    let mut on = d.marked.contains(&v.id);
                                    if ui.checkbox(&mut on, "").on_hover_text(tr!("打勾的會從 Jellyfin 刪除", "Ticked videos are deleted from Jellyfin")).changed() {
                                        if on {
                                            d.marked.insert(v.id.clone());
                                        } else {
                                            d.marked.remove(&v.id);
                                        }
                                    }
                                    let (r, _) = ui.allocate_exact_size(vec2(96.0, 54.0), Sense::hover());
                                    ui.painter().rect_filled(r, 6.0, p.surface2);
                                    match d.posters.get(&v.id) {
                                        Some(Some(tex)) => theme::paint_thumb_cover(ui.painter(), r, tex),
                                        Some(None) => {}
                                        None if v.has_image && ui.is_rect_visible(r) => want_posters.push(v.id.clone()),
                                        None => {}
                                    }
                                    if !matches!(d.posters.get(&v.id), Some(Some(_))) {
                                        theme::paint_icon(ui.painter(), Rect::from_center_size(r.center(), vec2(22.0, 22.0)), Icon::Video, p.muted);
                                    }
                                    ui.vertical(|ui| {
                                        ui.spacing_mut().item_spacing.y = 2.0;
                                        ui.horizontal(|ui| {
                                            ui.label(RichText::new(&v.name).font(theme::font_bold(13.0)));
                                            if i == 0 {
                                                theme::chip(ui, tr!("畫質最好", "Best quality"), theme::Tone::Ok, false);
                                            }
                                            if v.played {
                                                theme::chip(ui, tr!("看過", "Watched"), theme::Tone::Plain, false);
                                            }
                                        });
                                        let res = if v.width > 0 { format!("{}×{}", v.width, v.height) } else { String::new() };
                                        let meta: Vec<String> = [res, format_bytes(v.size), if v.added.is_empty() { String::new() } else { trf!("加入 {}", "Added {}", v.added) }]
                                            .into_iter()
                                            .filter(|s| !s.is_empty())
                                            .collect();
                                        ui.label(RichText::new(meta.join("・")).font(theme::font(12.0)).color(p.muted));
                                        ui.add(egui::Label::new(RichText::new(folder(&v.path)).font(theme::font(12.0)).color(p.muted)).truncate()).on_hover_text(&v.path);
                                    });
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if Btn::new(tr!("播放", "Play")).icon(Icon::Play).small().ghost().show(ui).clicked() {
                                            play_it = Some(v.clone());
                                        }
                                    });
                                });
                            });
                        }
                        ui.separator();
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let n = d.marked.len();
                    let label = trf!("從 Jellyfin 刪除打勾的 {n} 支（省下 {}）", "Delete the {n} ticked from Jellyfin (frees {})", format_bytes(saved));
                    if Btn::new(label).icon(Icon::Trash).danger().enabled(n > 0).show(ui).clicked() {
                        delete = true;
                    }
                    if Btn::new(tr!("全部不勾", "Untick all")).ghost().small().enabled(n > 0).show(ui).clicked() {
                        d.marked.clear();
                    }
                });
            }
        }
    });
    let scanning = matches!(d.phase, Phase::Scanning(_));
    if cancel || (scanning && (close || modal.should_close())) {
        if let Phase::Scanning(pr) = &d.phase {
            pr.cancel.store(true, Ordering::Relaxed);
        }
    }
    if !scanning && (close || (modal.should_close() && app.viewer.is_none() && app.ask.is_none())) {
        return;
    }
    let user = d.user.clone();
    let names: Vec<(String, String)> = match (&d.phase, delete) {
        (Phase::Done(groups), true) => groups.iter().flat_map(|g| g.items.iter()).filter(|v| d.marked.contains(&v.id)).map(|v| (v.id.clone(), v.name.clone())).collect(),
        _ => vec![],
    };
    app.jf_dupes = Some(d);
    for id in want_posters {
        load_poster(app, &id);
    }
    if let Some(v) = play_it {
        play(app, &user, &v);
    }
    if !names.is_empty() {
        super::jellyfin_browse::confirm_delete_list(app, names);
    }
}

/// 讀一格縮圖（背景下載、轉成材質）
fn load_poster(app: &mut UiApp, id: &str) {
    let Some(jf) = app.jellyfin() else { return };
    let Some(d) = app.jf_dupes.as_mut() else { return };
    d.posters.insert(id.to_string(), None);
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
            (id, img.map(|i| ctx.load_texture("jf-dupe-poster", i, egui::TextureOptions::LINEAR)))
        },
        |app, (id, tex)| {
            if let Some(d) = app.jf_dupes.as_mut() {
                d.posters.insert(id, tex);
            }
        },
    );
}
