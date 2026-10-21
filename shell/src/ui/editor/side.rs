//! 右側面板：時間（頭尾、刪除的片段）、畫面裁切、標註（工具、屬性、清單）；截圖另有「外觀與輸出」（外框、陰影、背景、大小、格式）。

use super::shot::Act;
use super::stage::paint_tool_icon;
use super::{ann_color, parse_time, Editor, Tab, Tool};
use crate::ui::dialogs::file_name;
use crate::ui::theme::{self, segmented, switch, Btn, Icon};
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, CornerRadius, CursorIcon, Id, Layout, RichText, Sense, Stroke};
use screenrecorder_core::annotate::{self, AnnKind, Shape, COLORS};
use screenrecorder_core::edit::{normalize_crop, normalize_ranges, CropInput};
use screenrecorder_core::format::{speed_label, video_clock};
use screenrecorder_core::idle;
use screenrecorder_core::picture;
use screenrecorder_core::shot_edit::BACKGROUNDS;
use screenrecorder_core::video_frame::{self, VideoFrame};
use screenrecorder_core::zoom;
use screenrecorder_core::{tr, trf};

pub fn show(ed: &mut Editor, ui: &mut egui::Ui, ctx: &egui::Context, toast: &mut Option<(String, bool)>) {
    if let Some(m) = ed.voice_msg.take() {
        *toast = Some(m);
    }
    tabs(ed, ui);
    ui.add_space(4.0);
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.set_width(ui.available_width() - 6.0);
        ui.spacing_mut().item_spacing = vec2(8.0, 10.0);
        match ed.tab {
            Tab::Time => time_panel(ed, ui, toast),
            Tab::Speed => super::speed::panel(ed, ui, toast),
            Tab::Crop => {
                crop_panel(ed, ui);
                if !ed.is_shot() {
                    let on = ed.spec.intro.as_ref().is_some_and(|c| !c.is_empty()) || ed.spec.outro.as_ref().is_some_and(|c| !c.is_empty());
                    fold(ed, ui, "cards", tr!("片頭、片尾", "Intro and outro"), on, |ed, ui| cards_panel(ed, ui, ctx));
                }
            }
            Tab::Ann => ann_panel(ed, ui, ctx),
            Tab::Output => super::shot::output_panel(ed, ui, ctx),
            Tab::Subs => {
                super::subs::tab(ed, ui, ctx, toast);
                // 章節常常是依字幕產生的，放在一起
                chapters_panel(ed, ui, toast);
            }
            Tab::Audio => {
                audio_panel(ed, ui);
                super::voice::panel(ed, ui, toast);
            }
        }
    });
}

fn tabs(ed: &mut Editor, ui: &mut egui::Ui) {
    let p = theme::pal(ui);
    // 標註與字幕分開算
    let subs = ed.sub_ids.len();
    let anns = ed.anns.len() - subs.min(ed.anns.len());
    let tabs: Vec<(Tab, &str, usize)> = if ed.is_shot() {
        vec![(Tab::Ann, tr!("標註", "Annotate"), anns), (Tab::Crop, tr!("裁切與旋轉", "Crop & rotate"), 0), (Tab::Output, tr!("外觀與輸出", "Look & output"), 0)]
    } else {
        vec![
            (Tab::Time, tr!("修剪", "Trim"), 0),
            (Tab::Speed, tr!("加速", "Speed"), ed.speed_count()),
            (Tab::Crop, tr!("畫面", "Frame"), 0),
            (Tab::Ann, tr!("標註", "Annotate"), anns),
            (Tab::Subs, tr!("字幕", "Subtitles"), subs),
            (Tab::Audio, tr!("聲音", "Audio"), 0),
        ]
    };
    let galleys: Vec<_> = tabs
        .iter()
        .map(|&(tab, label, n)| {
            let on = ed.tab == tab;
            let f = if on { theme::font_bold(13.0) } else { theme::font(13.0) };
            let g = ui.painter().layout_no_wrap(label.to_string(), f, if on { p.text } else { p.muted });
            let extra = (n > 0).then(|| ui.painter().layout_no_wrap(n.to_string(), theme::font_bold(13.0), p.accent));
            (tab, g, extra)
        })
        .collect();
    // 放不下時縮小左右留白（五個分頁、英文介面）
    let natural: f32 = galleys.iter().map(|(_, g, e)| g.size().x + e.as_ref().map(|e| e.size().x + 4.0).unwrap_or(0.0)).sum();
    let pad = ((ui.available_width() - natural - 2.0 * (galleys.len() as f32 - 1.0)) / galleys.len() as f32 / 2.0).clamp(5.0, 14.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (tab, g, extra) in galleys {
            let on = ed.tab == tab;
            let w = g.size().x + extra.as_ref().map(|e| e.size().x + 4.0).unwrap_or(0.0) + pad * 2.0;
            let (r, resp) = ui.allocate_exact_size(vec2(w, 32.0), Sense::click());
            let y = r.center().y - g.size().y / 2.0;
            let gw = g.size().x;
            ui.painter().galley(pos2(r.left() + pad, y), g, p.text);
            if let Some(extra) = extra {
                ui.painter().galley(pos2(r.left() + pad + 4.0 + gw, y), extra, p.accent);
            }
            if on {
                ui.painter().hline(r.x_range(), r.bottom() - 1.0, Stroke::new(2.0, p.accent));
            }
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                ed.tab = tab;
            }
        }
    });
    let r = ui.cursor();
    ui.painter().hline(r.x_range(), r.top() - ui.spacing().item_spacing.y + 0.5, Stroke::new(1.0, p.border));
}

pub(super) fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(theme::muted(ui, text).font(theme::font(12.0)));
}

pub(super) fn hint_text(ui: &mut egui::Ui, text: &str) {
    hint(ui, text);
}

/// 第一節的標題（沒有分隔線）
pub(super) fn section_title(ui: &mut egui::Ui, title: &str) {
    section(ui, title, false);
}

/// 有分隔線的小節標題
pub(super) fn section_title_sep(ui: &mut egui::Ui, title: &str) {
    section(ui, title, false);
}

fn time_panel(ed: &mut Editor, ui: &mut egui::Ui, toast: &mut Option<(String, bool)>) {
    let p = theme::pal(ui);
    // 三步驟就會剪：頭尾、中間、刪掉
    if ed.sel.is_none() {
        for (n, zh, en) in [
            (
                "1",
                "在縮圖上拖過不要的部分：開頭、中間、結尾都一樣（剪頭尾也可以拖兩端的黃色把手）",
                "Drag across the part you don't want on the thumbnails, whether it's the start, the middle or the end (you can also drag the yellow handles to trim)",
            ),
            ("2", "按選取範圍上方的「刪除」（或 Delete）；要加速就按「加速」", "Press “Delete” above the selection (or Delete). Press “Speed up” to speed it up instead"),
            ("3", "刪掉的部分在時間軸上收起來，點紅色的復原鈕可以放回來（Ctrl+Z 也可以）", "Removed parts fold away on the timeline. Click the red restore button to bring one back (or press Ctrl+Z)"),
        ] {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (r, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 9.0, p.accent.gamma_multiply(0.2));
                ui.painter().text(r.center(), Align2::CENTER_CENTER, n, theme::font_bold(11.5), p.accent);
                ui.add(egui::Label::new(RichText::new(tr!(zh, en)).font(theme::font(12.5))).wrap());
            });
        }
        ui.add_space(4.0);
    }
    ui.horizontal(|ui| {
        if Btn::new(tr!("設為開頭", "Set start")).small().tooltip(tr!("快速鍵 I", "Shortcut: I")).show(ui).clicked() {
            let t = ed.now();
            ed.set_start(t);
        }
        if Btn::new(tr!("設為結尾", "Set end")).small().tooltip(tr!("快速鍵 O", "Shortcut: O")).show(ui).clicked() {
            let t = ed.now();
            ed.set_end(t);
        }
    });
    ui.label(theme::muted(ui, trf!("開頭 {}　結尾 {}", "Start {} · End {}", video_clock(ed.spec.start), video_clock(ed.spec.end))));
    let all = ed.sels();
    if !all.is_empty() {
        egui::Frame::new().fill(Color32::from_rgba_unmultiplied(214, 140, 18, 31)).corner_radius(6).inner_margin(8).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.label(RichText::new(super::speed::selected_text(&all)).font(theme::font(12.5)));
            if all.len() == 1 {
                ui.label(theme::muted(ui, tr!("按住 Ctrl 再點（或拖曳）其他段，可以一次選好幾段", "Hold Ctrl and click (or drag) other sections to select several at once")).font(theme::font(11.5)));
            }
            ui.horizontal(|ui| {
                if Btn::new(if all.len() > 1 { tr!("刪除這幾段", "Delete sections") } else { tr!("刪除這段", "Delete section") })
                    .danger()
                    .small()
                    .tooltip(tr!("快速鍵 Delete", "Shortcut: Delete"))
                    .show(ui)
                    .clicked()
                {
                    ed.delete_selection(toast);
                }
                if Btn::new(tr!("取消選取", "Deselect")).ghost().small().tooltip(tr!("快速鍵 Esc", "Shortcut: Esc")).show(ui).clicked() {
                    ed.sel = None;
                }
            });
            if Btn::new(if all.len() > 1 { tr!("加速、放慢這幾段…", "Speed up or slow down…") } else { tr!("加速、放慢這段…", "Speed up or slow down…") })
                .small()
                .tooltip(tr!("到「加速」分頁設定這段的速度", "Set this section's speed in the Speed tab"))
                .show(ui)
                .clicked()
            {
                ed.tab = Tab::Speed;
            }
        });
    }
    let removed = normalize_ranges(&ed.spec.removed, ed.duration);
    if !removed.is_empty() {
        let mut restore = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            for (i, (a, b)) in removed.iter().enumerate() {
                let g = ui.painter().layout_no_wrap(trf!("刪除 {} – {}", "Removed {} – {}", video_clock(*a), video_clock(*b)), theme::mono(12.0), p.rec);
                let (r, _) = ui.allocate_exact_size(vec2(g.size().x + 10.0 + 24.0, 24.0), Sense::hover());
                ui.painter().rect_filled(r, CornerRadius::same(12), p.rec_soft);
                ui.painter().galley(pos2(r.left() + 10.0, r.center().y - g.size().y / 2.0), g, p.rec);
                let x = egui::Rect::from_center_size(pos2(r.right() - 13.0, r.center().y), vec2(20.0, 20.0));
                let xr = ui.interact(x, Id::new(("ed-restore", i)), Sense::click()).on_hover_text(tr!("還原這段", "Restore this section")).on_hover_cursor(CursorIcon::PointingHand);
                if xr.hovered() {
                    ui.painter().circle_filled(x.center(), 10.0, p.rec.gamma_multiply(0.2));
                }
                ui.painter().text(x.center(), Align2::CENTER_CENTER, "×", theme::font(14.0), p.rec);
                if xr.clicked() {
                    restore = Some(i);
                }
            }
        });
        if let Some(i) = restore {
            ed.restore_removed(i);
        }
        // 接縫淡入淡出：刪掉片段的地方不會突然跳過去
        theme::check(ui, &mut ed.spec.fade_cuts, RichText::new(tr!("接縫淡入淡出", "Fade at cuts")).font(theme::font(12.5))).on_hover_text(tr!(
            "刪掉片段的地方，畫面和聲音先淡出、再淡入（各 0.2 秒），不會突然跳過去或「啪」一聲。輸出時才會套用",
            "Where a section was removed, the picture and sound fade out and back in (0.2 s each) instead of jumping or popping. Applied when you save"
        ));
    }
    // 片段順序：保留兩段以上時可以調整前後
    let order = ed.ordered_keep();
    if order.len() >= 2 {
        section(ui, tr!("片段順序", "Section order"), false);
        let mut mv = None;
        for (i, (a, b)) in order.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(RichText::new(format!("{}.", i + 1)).font(theme::font_bold(12.5)));
                ui.label(RichText::new(format!("{} – {}", video_clock(*a), video_clock(*b))).font(theme::mono(12.0)));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if Btn::new("↓").small().ghost().enabled(i + 1 < order.len()).tooltip(tr!("往後移", "Move later")).show(ui).clicked() {
                        mv = Some((i, 1));
                    }
                    if Btn::new("↑").small().ghost().enabled(i > 0).tooltip(tr!("往前移", "Move earlier")).show(ui).clicked() {
                        mv = Some((i, -1));
                    }
                });
            });
        }
        if let Some((i, d)) = mv {
            ed.move_segment(i, d);
        }
        if ed.is_reordered() {
            ui.horizontal(|ui| {
                if Btn::new(tr!("恢復原本順序", "Restore original order")).small().ghost().show(ui).clicked() {
                    ed.spec.order.clear();
                }
                hint(ui, tr!("按「預覽結果」會照新的順序播放。", "“Preview result” plays in the new order."));
            });
        }
    }
    hint(ui, tr!("加速、放慢、定格在「加速」分頁；章節在「字幕」分頁。", "Speed changes and freeze frames are in the Speed tab; chapters are in the Subtitles tab."));
    cleanup_panel(ed, ui, toast);
}

/// 章節：標記清單（跳過去、改名、刪除），加標記、依字幕產生、複製成 YouTube 說明欄的格式
fn chapters_panel(ed: &mut Editor, ui: &mut egui::Ui, toast: &mut Option<(String, bool)>) {
    section(ui, tr!("章節", "Chapters"), false);
    let has_subs = !ed.sub_ids.is_empty();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
        if Btn::new(tr!("＋ 在播放頭加標記（M）", "+ Marker at playhead (M)")).small().enabled(ed.duration > 0.0).show(ui).clicked() {
            *toast = Some(ed.add_marker());
        }
        if Btn::new(tr!("依字幕產生", "From subtitles"))
            .small()
            .enabled(has_subs)
            .tooltip(if has_subs {
                tr!(
                    "在講話停頓最久的地方切成幾章（每章至少 30 秒），用那一章第一句當標題",
                    "Split into chapters at the longest pauses in speech (at least 30 s each), titled with each chapter's first line"
                )
            } else {
                tr!("要先在「字幕」分頁產生字幕", "Generate subtitles in the Subtitles tab first")
            })
            .show(ui)
            .clicked()
        {
            *toast = Some((ed.chapters_from_subs(), false));
        }
        if !ed.markers.is_empty()
            && Btn::new(tr!("複製（貼到 YouTube）", "Copy (for YouTube)"))
                .small()
                .ghost()
                .tooltip(tr!("依剪輯後的時間複製成 YouTube 說明欄的章節格式", "Copy as YouTube description chapters, using the edited timing"))
                .show(ui)
                .clicked()
        {
            let named = screenrecorder_core::chapters::from_named(&ed.markers, ed.duration);
            let shift = ed.spec.intro.as_ref().filter(|c| !c.is_empty()).map_or(0.0, |c| c.secs());
            let mapped: Vec<_> = screenrecorder_core::chapters::map_cut(&named, &ed.parts()).into_iter().map(|c| screenrecorder_core::types::Chapter { start: c.start + shift, ..c }).collect();
            let total = screenrecorder_core::edit::output_length(&ed.parts()) + shift;
            ui.ctx().copy_text(screenrecorder_core::chapters::youtube_text(&mapped, total));
            *toast = Some((tr!("已複製章節，可以貼到 YouTube 影片的說明欄", "Chapters copied. Paste them into the YouTube video description").into(), false));
        }
    });
    if ed.markers.is_empty() {
        return hint(
            ui,
            tr!("標記會在輸出時變成影片的章節。也可以按播放列上的「＋ 標記」或按 M。", "Markers become chapters in the output video. You can also press “+ Marker” under the timeline or M."),
        );
    }
    let p = theme::pal(ui);
    let (mut seek, mut rename, mut remove) = (None, None, None);
    for i in 0..ed.markers.len() {
        let t = ed.markers[i].t;
        let name = ed.marker_name(i);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let r = ui
                .add(egui::Label::new(RichText::new(video_clock(t)).font(theme::mono(12.0)).color(p.accent)).sense(Sense::click()))
                .on_hover_text(tr!("跳到這裡", "Jump here"))
                .on_hover_cursor(CursorIcon::PointingHand);
            if r.clicked() {
                seek = Some(t);
            }
            ui.add(egui::Label::new(RichText::new(name).font(theme::font(12.5))).truncate());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if Btn::icon_only(theme::Icon::Trash).ghost().small().tooltip(tr!("刪除", "Delete")).show(ui).clicked() {
                    remove = Some(i);
                }
                if Btn::icon_only(theme::Icon::Rename).ghost().small().tooltip(tr!("改名", "Rename")).show(ui).clicked() {
                    rename = Some(i);
                }
            });
        });
    }
    if let Some(t) = seek {
        ed.seek(t);
    }
    if let Some(i) = rename {
        ed.vpending = Some(super::VAct::RenameMarker(i));
    }
    if let Some(i) = remove {
        ed.markers.remove(i);
    }
}

/// 清掉空白：沒動靜（畫面不動又沒聲音）、沒說話（畫面在動）、贅字和停頓（依字幕）一起找，
/// 找到的標在時間軸上，再選刪除或加速
pub(super) fn cleanup_panel(ed: &mut Editor, ui: &mut egui::Ui, toast: &mut Option<(String, bool)>) {
    section(ui, tr!("清掉空白", "Clean up blank parts"), false);
    let busy = ed.finding_idle;
    let has_audio = ed.entry.media.has_audio == Some(true);
    let has_subs = !ed.sub_ids.is_empty();
    hint(ui, tr!("勾選要找的，找到的地方會標在時間軸上，再決定刪除或加速。", "Choose what to look for. What's found is marked on the timeline, then you decide whether to remove or speed it up."));
    let kinds: [(String, String, bool); 3] = [
        (tr!("沒動靜", "Idle").into(), trf!("畫面不動、也沒有聲音超過 {} 秒（等待、發呆）", "The picture is still and silent for more than {} s (waiting, pauses)", idle::MIN_IDLE), true),
        (
            tr!("沒說話", "Quiet").into(),
            if has_audio {
                trf!("畫面在動、但超過 {} 秒沒講話（打字、捲動、等待）", "The picture moves but nobody speaks for more than {} s (typing, scrolling, waiting)", idle::MIN_QUIET)
            } else {
                tr!("這支影片沒有聲音", "This video has no sound").into()
            },
            has_audio,
        ),
        (
            tr!("贅字和停頓", "Fillers and pauses").into(),
            if has_subs {
                tr!("只有「嗯、呃、那個」的句子，和句子之間超過 1 秒的停頓（依字幕）", "Sentences that are only “um”, “uh” and the like, and gaps of over 1 s between sentences (from subtitles)")
                    .into()
            } else {
                tr!("要先在「字幕」分頁產生字幕", "Generate subtitles in the Subtitles tab first").into()
            },
            has_subs,
        ),
    ];
    for (i, (label, tip, can)) in kinds.iter().enumerate() {
        ui.add_enabled_ui(*can, |ui| {
            let mut on = ed.clean_kinds[i] && *can;
            ui.horizontal(|ui| {
                if theme::check(ui, &mut on, RichText::new(label.as_str()).font(theme::font(12.5))).on_hover_text(tip.as_str()).changed() {
                    ed.clean_kinds[i] = on;
                }
                ui.add(egui::Label::new(theme::muted(ui, tip.as_str()).font(theme::font(11.5))).wrap());
            });
        });
    }
    let want = [ed.clean_kinds[0], ed.clean_kinds[1] && has_audio, ed.clean_kinds[2] && has_subs];
    let any = want.iter().any(|w| *w);
    let tip = if any { "" } else { tr!("至少勾選一種", "Choose at least one") };
    if Btn::new(if busy { tr!("正在分析…", "Analyzing…") } else { tr!("找出來", "Find them") }).small().enabled(!busy && any && ed.duration > 0.0).tooltip(tip).show(ui).clicked() {
        ed.idle_found = None;
        ed.fillers = None;
        if want[2] {
            let found = ed.find_fillers();
            if !found.is_empty() {
                ed.fillers = Some(
                    found
                        .into_iter()
                        .map(|f| {
                            let on = f.default_on();
                            (f, on)
                        })
                        .collect(),
                );
            }
        }
        if want[0] || want[1] {
            ed.pending = Some(super::shot::Act::FindIdle);
        } else if ed.fillers.is_none() {
            *toast = Some((tr!("沒有找到贅字或停頓", "No fillers or pauses found").into(), false));
        }
    }
    found_panel(ed, ui, toast);
    super::subs::fillers_panel(ed, ui, toast);
}

/// 分析的結果：兩種片段各一列（幾段、共多久），按鈕刪除或加速；時間軸上用顏色標出來
pub(super) fn found_panel(ed: &mut Editor, ui: &mut egui::Ui, toast: &mut Option<(String, bool)>) {
    let Some(found) = ed.idle_found.clone() else { return };
    let total = |v: &[(f64, f64)]| v.iter().map(|(a, b)| b - a).sum::<f64>();
    egui::Frame::new().fill(theme::pal(ui).surface2).corner_radius(6).inner_margin(8).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 6.0;
        if !found.idle.is_empty() {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                ui.painter().rect_filled(r, CornerRadius::same(2), super::timeline::IDLE_MARK);
                ui.label(RichText::new(trf!("不動又沒聲音：{} 段，共 {}", "Still and silent: {}, {} in total", found.idle.len(), video_clock(total(&found.idle)))).font(theme::font(12.5)));
            });
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if Btn::new(tr!("全部刪除", "Remove all")).danger().small().show(ui).clicked() {
                    *toast = Some((ed.remove_idle(&found.idle), false));
                    if let Some(f) = &mut ed.idle_found {
                        f.idle.clear();
                    }
                }
                for s in [4u32, 8] {
                    if Btn::new(format!("{s}×")).small().tooltip(trf!("這些片段以 {s} 倍速播放（沒有聲音）", "Play these parts at {s}× speed (no sound)")).show(ui).clicked() {
                        *toast = Some((ed.speed_found(&found.idle, s), false));
                        if let Some(f) = &mut ed.idle_found {
                            f.idle.clear();
                        }
                    }
                }
            });
        }
        if !found.quiet.is_empty() {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                ui.painter().rect_filled(r, CornerRadius::same(2), super::timeline::QUIET_MARK);
                ui.label(
                    RichText::new(trf!("沒說話（畫面在動）：{} 段，共 {}", "Quiet (picture moving): {}, {} in total", found.quiet.len(), video_clock(total(&found.quiet)))).font(theme::font(12.5)),
                );
            });
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(theme::muted(ui, tr!("加速", "Speed up")).font(theme::font(12.0)));
                for s in [2u32, 4, 8] {
                    if Btn::new(format!("{s}×"))
                        .small()
                        .tooltip(trf!("打字、捲動、等待的地方以 {s} 倍速帶過（沒有聲音）", "Play typing, scrolling and waiting at {s}× speed (no sound)"))
                        .show(ui)
                        .clicked()
                    {
                        *toast = Some((ed.speed_found(&found.quiet, s), false));
                        if let Some(f) = &mut ed.idle_found {
                            f.quiet.clear();
                        }
                    }
                }
                if Btn::new(tr!("刪除", "Remove")).ghost().small().show(ui).clicked() {
                    *toast = Some((ed.remove_idle(&found.quiet), false));
                    if let Some(f) = &mut ed.idle_found {
                        f.quiet.clear();
                    }
                }
            });
        }
        if Btn::new(tr!("不用了，清除標示", "Dismiss")).ghost().small().show(ui).clicked() {
            ed.idle_found = None;
        }
    });
    if ed.idle_found.as_ref().is_some_and(|f| f.idle.is_empty() && f.quiet.is_empty()) {
        ed.idle_found = None;
    }
}

/// 「聲音」分頁：聲音處理（輸出時套用）
fn audio_panel(ed: &mut Editor, ui: &mut egui::Ui) {
    let has = ed.entry.media.has_audio == Some(true);
    section(ui, tr!("聲音處理", "Audio processing"), true);
    if !has {
        return hint(ui, tr!("這支影片沒有聲音。", "This video has no audio."));
    }
    let fx = &mut ed.spec.audio;
    switch(ui, &mut fx.mute, tr!("不要聲音", "Remove audio"), true).on_hover_text(tr!("輸出的影片沒有聲音", "The output video has no sound"));
    let tracks = ed.entry.media.audio_tracks >= 3;
    ui.add_enabled_ui(!fx.mute, |ui| {
        // 分軌錄影：系統聲音與麥克風分別調整
        if tracks {
            let mut on = fx.tracks.is_some();
            if switch(ui, &mut on, tr!("分別調整系統聲音與麥克風", "Adjust system audio and mic separately"), true)
                .on_hover_text(tr!(
                    "這支是分軌錄影：可以把背景音樂調小、講話的聲音調大，降噪也只處理麥克風",
                    "This recording has separate tracks: turn background music down and your voice up; noise reduction only applies to the mic"
                ))
                .changed()
            {
                fx.tracks = on.then(screenrecorder_core::edit::TrackMix::default);
            }
            if let Some(t) = &mut fx.tracks {
                for (label, v) in [(tr!("系統聲音", "System audio"), &mut t.system), (tr!("麥克風", "Microphone"), &mut t.mic)] {
                    ui.horizontal(|ui| {
                        ui.add_sized(vec2(64.0, 20.0), egui::Label::new(theme::muted(ui, label).font(theme::font(12.0))));
                        ui.spacing_mut().slider_width = (ui.available_width() - 70.0).max(80.0).floor();
                        theme::slider_style(ui);
                        ui.add(egui::Slider::new(v, 0..=200).step_by(5.0).suffix("%"));
                    });
                }
            }
        }
        let denoise_tip = if fx.tracks.is_some() {
            tr!("減少麥克風收到的風扇、冷氣等持續的背景雜音（系統聲音不處理）", "Reduce constant background noise (fans, air conditioning) picked up by the mic; system audio isn't affected")
        } else {
            tr!("減少風扇、冷氣等持續的背景雜音", "Reduce constant background noise such as fans or air conditioning")
        };
        switch(ui, &mut fx.denoise, tr!("降噪", "Noise reduction"), true).on_hover_text(denoise_tip);
        let voice_tip = if fx.tracks.is_some() {
            tr!("濾掉低沉的嗡嗡聲、讓咬字更清楚、大小聲更平均（只處理麥克風）", "Remove low rumble, make speech clearer and even out loud and quiet words (mic only)")
        } else {
            tr!("濾掉低沉的嗡嗡聲、讓咬字更清楚、大小聲更平均", "Remove low rumble, make speech clearer and even out loud and quiet words")
        };
        switch(ui, &mut fx.voice, tr!("人聲加強", "Enhance voice"), true).on_hover_text(voice_tip);
        switch(ui, &mut fx.normalize, tr!("音量平衡", "Normalize loudness"), true)
            .on_hover_text(tr!("忽大忽小的音量變平均，整體調到適合聆聽的大小", "Even out loud and quiet parts and set a comfortable overall volume"));
    });
    hint(ui, tr!("預覽時聽到的是原本的聲音，輸出的影片才會套用。", "The preview plays the original audio; these settings apply to the output video only."));
}

/// 分頁裡的小節標題（第一節不畫分隔線）
pub(super) fn section(ui: &mut egui::Ui, title: &str, first: bool) {
    if !first {
        let p = theme::pal(ui);
        ui.add_space(4.0);
        let r = ui.cursor();
        ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0, p.border));
        ui.add_space(6.0);
    }
    ui.label(RichText::new(title).font(theme::font_bold(13.5)));
}

/// 「裁切與旋轉」（截圖）／「畫面裁切」（影片）：旋轉只在這裡；裁切直接在圖上拖曳，不用先打開開關
fn crop_panel(ed: &mut Editor, ui: &mut egui::Ui) {
    let shot = ed.is_shot();
    if shot {
        section(ui, tr!("旋轉", "Rotate"), true);
        ui.horizontal(|ui| super::shot::rotate_buttons(ed, ui));
    }
    section(ui, tr!("裁切", "Crop"), !shot);
    let can = ed.vw > 0.0;
    if !can {
        return hint(ui, &trf!("無法讀取{}尺寸，不能裁切。", "Couldn't read the {} size, so it can't be cropped.", ed.what()));
    }
    if !shot && ed.vertical == 2 {
        // 跟著游標的直式影片自己會裁出 9:16 的一塊
        warn_hint(ui, tr!("「直式影片：跟著游標」會自己裁出 9:16 的一塊，不另外裁切畫面。", "“Vertical video: Follow cursor” crops its own 9:16 window, so manual cropping isn't used."));
    } else {
        crop_controls(ed, ui);
    }
    if !shot {
        // 其他效果收起來：有開的才展開（標題旁邊顯示「已開啟」），畫面分頁不會一長串
        let on = ed.zoom > 1.0 || !ed.spec.zoom_regions.is_empty() || ed.cursor_size > 0.0;
        fold(ed, ui, "cursor", tr!("游標與放大", "Cursor and zoom"), on, |ed, ui| {
            zoom_panel(ed, ui);
            region_panel(ed, ui);
            cursor_panel(ed, ui);
        });
        let on = !ed.spec.look.is_default();
        fold(ed, ui, "look", tr!("調色", "Color"), on, look_panel);
        let on = ed.spec.stamp.is_some();
        fold(ed, ui, "stamp", tr!("日期時間", "Date and time"), on, stamp_panel);
        let on = ed.vertical != 0;
        fold(ed, ui, "vertical", tr!("直式影片（9:16）", "Vertical video (9:16)"), on, vertical_panel);
        let on = ed.spec.frame.is_some();
        fold(ed, ui, "frame", tr!("背景與圓角", "Background and rounded corners"), on, frame_panel);
    }
}

/// 小標題（收合區塊裡面的一小節）
fn sub_title(ui: &mut egui::Ui, title: &str) {
    ui.add_space(2.0);
    ui.label(RichText::new(title).font(theme::font_bold(12.5)));
}

/// 可以收起來的區塊：標題列點一下展開／收起；有開啟的效果時預設展開，標題旁邊標「已開啟」
pub(super) fn fold(ed: &mut Editor, ui: &mut egui::Ui, key: &str, title: &str, on: bool, body: impl FnOnce(&mut Editor, &mut egui::Ui)) {
    let p = theme::pal(ui);
    let id = Id::new(("ed-fold", key));
    let mut open = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(on);
    ui.add_space(2.0);
    let r = ui.cursor();
    ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0, p.border));
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 30.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(4), p.surface2);
    }
    let icon = egui::Rect::from_center_size(pos2(rect.left() + 9.0, rect.center().y), vec2(14.0, 14.0));
    theme::paint_icon(ui.painter(), icon, if open { theme::Icon::ChevD } else { theme::Icon::ChevR }, p.muted);
    let g = ui.painter().layout_no_wrap(title.to_string(), theme::font_bold(13.5), p.text);
    let tx = rect.left() + 22.0;
    let gw = g.size().x;
    ui.painter().galley(pos2(tx, rect.center().y - g.size().y / 2.0), g, p.text);
    if on {
        let b = ui.painter().layout_no_wrap(tr!("已開啟", "On").to_string(), theme::font(11.5), p.accent);
        let br = egui::Rect::from_min_size(pos2(tx + gw + 8.0, rect.center().y - b.size().y / 2.0 - 2.0), b.size() + vec2(12.0, 4.0));
        ui.painter().rect_filled(br, CornerRadius::same(8), p.accent.gamma_multiply(0.15));
        ui.painter().galley(br.min + vec2(6.0, 2.0), b, p.accent);
    }
    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
        open = !open;
        ui.data_mut(|d| d.insert_temp(id, open));
    }
    if open {
        body(ed, ui);
        ui.add_space(4.0);
    }
}

/// 比例、目前的範圍、精確數值
fn crop_controls(ed: &mut Editor, ui: &mut egui::Ui) {
    // 比例：選了就在目前的範圍裡放一個最大的那個比例，之後拖曳也保持比例
    ui.allocate_ui_with_layout(vec2(ui.available_width(), 34.0), Layout::left_to_right(Align::Center), |ui| {
        ui.label(theme::muted(ui, tr!("比例", "Ratio")).font(theme::font(12.0)));
        let items: Vec<(usize, &str)> = super::CROP_RATIOS.iter().enumerate().map(|(i, r)| (i, tr!(r.0, r.1))).collect();
        if segmented(ui, &mut ed.crop_ratio, &items, true) {
            ed.apply_crop_ratio();
        }
    });
    if ed.crop_on {
        let c = normalize_crop(ed.spec.crop, ed.vw as i32, ed.vh as i32);
        ui.horizontal(|ui| {
            if let Some(c) = c {
                ui.label(tr!("保留 ", "Size "));
                ui.label(RichText::new(format!("{}×{}", c.width, c.height)).font(theme::font_bold(13.5)));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if Btn::new(tr!("取消裁切", "Remove crop")).small().tooltip(tr!("回到完整的畫面（可以按 Ctrl+Z 復原）", "Go back to the full frame (press Ctrl+Z to undo)")).show(ui).clicked()
                {
                    ed.crop_on = false;
                }
            });
        });
        hint(ui, tr!("拖曳框內可以移動，拖曳四個角可以調整大小；在框外拖曳會重新框選。", "Drag inside the box to move it or drag a corner to resize it; drag outside the box to select again."));
    } else {
        hint(ui, &trf!("直接在{}上拖曳，框出要保留的範圍。", "Drag on the {} to select the area to keep.", ed.what()));
    }
    // 精確數值：大多數人用拖的，預設收起來
    egui::CollapsingHeader::new(RichText::new(tr!("輸入精確數值", "Enter exact values")).font(theme::font(12.5))).id_salt("ed-crop-exact").default_open(false).show(ui, |ui| {
        let c = ed.crop_rect();
        let mut v = [c.x, c.y, c.width, c.height];
        let mut changed = false;
        egui::Grid::new("ed-crop").num_columns(2).spacing(vec2(12.0, 8.0)).show(ui, |ui| {
            let labels = tr!(["X", "Y", "寬度", "高度"], ["X", "Y", "Width", "Height"]);
            let maxes = [ed.vw - 16.0, ed.vh - 16.0, ed.vw, ed.vh];
            for (i, l) in labels.iter().enumerate() {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(theme::muted(ui, *l).font(theme::font(12.0)));
                    let min = if i >= 2 { 16.0 } else { 0.0 };
                    if ui.add_sized(vec2(110.0, 26.0), egui::DragValue::new(&mut v[i]).speed(2.0).range(min..=maxes[i].max(min)).fixed_decimals(0)).changed() {
                        changed = true;
                    }
                });
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
        });
        if changed {
            ed.crop_on = true;
            ed.crop_ratio = 0;
            ed.set_crop(CropInput { x: v[0], y: v[1], width: v[2], height: v[3] });
        }
    });
}

/// 用不了的原因（橘色字）
fn warn_hint(ui: &mut egui::Ui, text: &str) {
    let p = theme::pal(ui);
    ui.label(RichText::new(text).font(theme::font(12.0)).color(p.warn));
}

/// 背景與圓角：影片縮小放在背景中間（輸出大小不變），四角變圓、有陰影
/// 調色：亮度、對比、飽和度（預覽看得到）、銳利化（輸出才有）
fn look_panel(ed: &mut Editor, ui: &mut egui::Ui) {
    use screenrecorder_core::look::{RANGE, SHARPEN_MAX};
    let look = &mut ed.spec.look;
    for (v, label) in [(&mut look.brightness, tr!("亮度", "Brightness")), (&mut look.contrast, tr!("對比", "Contrast")), (&mut look.saturation, tr!("飽和度", "Saturation"))] {
        ui.horizontal(|ui| {
            ui.add_sized(vec2(64.0, 20.0), egui::Label::new(theme::muted(ui, label).font(theme::font(12.0))));
            ui.spacing_mut().slider_width = (ui.available_width() - 70.0).max(80.0).floor();
            theme::slider_style(ui);
            ui.add(egui::Slider::new(v, -RANGE..=RANGE).step_by(1.0));
        });
    }
    ui.horizontal(|ui| {
        ui.add_sized(vec2(64.0, 20.0), egui::Label::new(theme::muted(ui, tr!("銳利", "Sharpen")).font(theme::font(12.0))));
        let items: Vec<(u32, &str)> = (0..=SHARPEN_MAX).map(|i| (i, [tr!("不用", "Off"), tr!("一點", "Low"), tr!("中", "Medium"), tr!("強", "High")][i as usize])).collect();
        segmented(ui, &mut look.sharpen, &items, true);
    });
    if look.is_default() {
        hint(
            ui,
            tr!(
                "錄影太暗、太灰，或攝影機偏色時調整；銳利化讓文字邊緣更清楚（只在輸出時處理）。",
                "Adjust when a recording is too dark or dull, or the camera's colors are off. Sharpen makes text edges crisper (applied on export)."
            ),
        );
    } else if Btn::new(tr!("恢復原本的顏色", "Reset colors")).small().show(ui).clicked() {
        *look = Default::default();
    }
}

/// 日期時間浮水印：畫面角落顯示錄影當時的時間
fn stamp_panel(ed: &mut Editor, ui: &mut egui::Ui) {
    let start = ed.rec_start_ms();
    let mut on = ed.spec.stamp.is_some();
    if switch(ui, &mut on, tr!("顯示錄影當時的日期時間", "Show when it was recorded"), start.is_some())
        .on_hover_text(tr!(
            "在畫面角落顯示錄影當下的日期與時間，適合存證、客服紀錄或操作稽核；剪掉片段、改順序後顯示的仍是錄影當時的時間",
            "Shows the date and time of recording in a corner, for evidence, support records or audits. After cutting or reordering, it still shows the original recording time"
        ))
        .changed()
    {
        ed.spec.stamp = on.then(screenrecorder_core::look::Stamp::default);
    }
    let Some(st) = &mut ed.spec.stamp else {
        if start.is_none() {
            hint(
                ui,
                tr!(
                    "不知道這支影片是什麼時候錄的（檔名不是錄影時間，也讀不到修改時間）。",
                    "It isn't known when this video was recorded (the file name isn't a recording time and there's no modified time)."
                ),
            );
        }
        return;
    };
    let corners = [(0u8, tr!("右下", "Bottom right")), (1, tr!("左下", "Bottom left")), (2, tr!("右上", "Top right")), (3, tr!("左上", "Top left"))];
    segmented(ui, &mut st.corner, &corners, true);
    let sizes = [(0u8, tr!("小", "Small")), (1, tr!("中", "Medium")), (2, tr!("大", "Large"))];
    segmented(ui, &mut st.size, &sizes, true);
    if let Some(ms) = start {
        hint_text(ui, &trf!("從 {} 開始（暫停的時間不算）", "Starts at {} (paused time isn't counted)", screenrecorder_core::look::stamp_preview(ms, 0.0)));
    }
}

fn frame_panel(ed: &mut Editor, ui: &mut egui::Ui) {
    let p = theme::pal(ui);
    let mut on = ed.spec.frame.is_some();
    if ed.vertical != 0 {
        // 用不了時說明原因（直式影片已經有自己的背景）
        warn_hint(
            ui,
            tr!("直式影片時不能使用背景與圓角（要用的話先把直式影片改成「不用」）。", "Background and rounded corners can't be used with vertical video (set Vertical video to “Off” first)."),
        );
    }
    if switch(ui, &mut on, tr!("放在背景上", "Place on a background"), ed.vw > 0.0 && ed.vertical == 0)
        .on_hover_text(tr!(
            "影片縮小放在漸層或單色背景中間，四角變圓、下方有陰影；輸出的大小不變",
            "The video shrinks onto a gradient or solid background, with rounded corners and a shadow below; the output size stays the same"
        ))
        .changed()
    {
        ed.spec.frame = on.then(VideoFrame::default);
    }
    let Some(mut f) = ed.spec.frame.clone() else {
        return hint(ui, tr!("適合放到簡報、社群或產品介紹：影片看起來像一張卡片。", "Great for presentations, social media or product demos: the video looks like a card."));
    };
    if ed.vertical != 0 {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        for bg in BACKGROUNDS {
            let (r, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click());
            super::shot::gradient_swatch(ui.painter(), r.shrink(2.0), bg);
            if f.background == bg {
                ui.painter().rect_stroke(r.expand(1.0), CornerRadius::same(7), Stroke::new(2.0, p.accent), egui::StrokeKind::Inside);
            }
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                f.background = bg.to_string();
            }
        }
    });
    // 和標籤放在同一行：英文用 S / M / L 才放得下
    let names = tr!(["無", "小", "中", "大"], ["None", "S", "M", "L"]);
    let items: Vec<(u32, &str)> = video_frame::RADII.iter().copied().zip(names).collect();
    ui.horizontal(|ui| {
        ui.label(theme::muted(ui, tr!("圓角", "Corners")).font(theme::font(12.0)));
        segmented(ui, &mut f.radius, &items, true);
    });
    let items: Vec<(u32, &str)> = video_frame::PADDINGS.iter().copied().zip(tr!(["少", "中", "多"], ["S", "M", "L"])).collect();
    ui.horizontal(|ui| {
        ui.label(theme::muted(ui, tr!("留白", "Padding")).font(theme::font(12.0)));
        segmented(ui, &mut f.padding, &items, true);
    });
    if ed.spec.frame.as_ref() != Some(&f) {
        ed.spec.frame = Some(f.clone());
    }
    // 預覽：目前這一格放在背景上的樣子（與輸出用同一套畫法）
    let crop = if ed.crop_on { normalize_crop(ed.spec.crop, ed.vw as i32, ed.vh as i32) } else { None };
    let (ow, oh) = crop.map(|c| (c.width as f64, c.height as f64)).unwrap_or((ed.vw, ed.vh));
    let pw = ui.available_width().min(320.0);
    let ph = (pw as f64 * oh / ow.max(1.0)) as f32;
    let key = super::hash_of(&(serde_json::to_string(&f).unwrap_or_default(), ed.video_key, pw as u32, crop.map(|c| (c.x, c.y, c.width, c.height))));
    if ed.frame_preview.as_ref().is_none_or(|(k, _)| *k != key) {
        if let Some(pm) = frame_preview(ed, &f, crop, pw as i32, ph as i32) {
            let img = egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data());
            ed.frame_preview = Some((key, ui.ctx().load_texture("frame-preview", img, egui::TextureOptions::LINEAR)));
        }
    }
    if let Some((_, tex)) = &ed.frame_preview {
        let (r, _) = ui.allocate_exact_size(vec2(pw, ph), Sense::hover());
        ui.painter().image(tex.id(), r, egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    hint(ui, tr!("上面是輸出的樣子（標註會一起縮小）；輸出的大小不變。", "Above is how the output will look (annotations shrink too); the output size stays the same."));
}

/// 背景與圓角的預覽圖（w × h）：底圖 + 目前這一格（縮小、裁切）+ 遮罩
fn frame_preview(ed: &Editor, f: &VideoFrame, crop: Option<screenrecorder_core::types::Rect>, w: i32, h: i32) -> Option<tiny_skia::Pixmap> {
    let frame = ed.frame.as_ref()?;
    let (mut out, cover) = video_frame::render(f, w.max(16), h.max(16))?;
    let l = video_frame::layout(f, w.max(16), h.max(16));
    let mut src = tiny_skia::Pixmap::new(frame.width, frame.height)?;
    src.data_mut().copy_from_slice(&frame.rgba[..(frame.width * frame.height * 4) as usize]);
    // 解出的畫面可能比原影片小：裁切範圍換算成畫面像素
    let (sx, sy) = (frame.width as f32 / ed.vw.max(1.0) as f32, frame.height as f32 / ed.vh.max(1.0) as f32);
    let (cx, cy, cw, ch) = match crop {
        Some(c) => (c.x as f32 * sx, c.y as f32 * sy, c.width as f32 * sx, c.height as f32 * sy),
        None => (0.0, 0.0, frame.width as f32, frame.height as f32),
    };
    let t = tiny_skia::Transform::from_translate(l.x as f32, l.y as f32).pre_scale(l.w as f32 / cw.max(1.0), l.h as f32 / ch.max(1.0)).pre_translate(-cx, -cy);
    let paint = tiny_skia::PixmapPaint { quality: tiny_skia::FilterQuality::Bilinear, ..Default::default() };
    let clip = tiny_skia::Rect::from_xywh(l.x as f32, l.y as f32, l.w as f32, l.h as f32).map(tiny_skia::PathBuilder::from_rect)?;
    let mut mask = tiny_skia::Mask::new(out.width(), out.height())?;
    mask.fill_path(&clip, tiny_skia::FillRule::Winding, false, tiny_skia::Transform::identity());
    out.draw_pixmap(0, 0, src.as_ref(), &paint, t, Some(&mask));
    out.draw_pixmap(0, 0, cover.as_ref(), &tiny_skia::PixmapPaint::default(), tiny_skia::Transform::identity(), None);
    Some(out)
}

/// 跟著點擊放大：錄影時記下的點擊，輸出時放大到點擊的地方
/// 手動放大一塊：在影片上框選，設定從幾秒到幾秒
fn region_panel(ed: &mut Editor, ui: &mut egui::Ui) {
    sub_title(ui, tr!("手動放大一塊", "Zoom into an area"));
    let can = !ed.crop_on && ed.vertical == 0;
    if !can {
        let why = if ed.vertical != 0 {
            tr!("直式影片時不能使用（要用的話先把直式影片改成「不用」）。", "Can't be used with vertical video (set Vertical video to “Off” first).")
        } else {
            tr!("裁切畫面時不能使用（要用的話先按「取消裁切」）。", "Can't be used while the video is cropped (press “Remove crop” first).")
        };
        warn_hint(ui, why);
    }
    if ed.zoom_pick {
        ui.label(
            RichText::new(tr!("在影片上拖曳，框出要放大的地方（按 Esc 取消）", "Drag on the video to frame the area to zoom into (Esc to cancel)"))
                .font(theme::font_bold(12.5))
                .color(theme::pal(ui).accent),
        );
        if Btn::new(tr!("取消", "Cancel")).small().ghost().show(ui).clicked() {
            ed.zoom_pick = false;
        }
    } else if Btn::new(tr!("在播放點框選要放大的地方", "Frame an area at the playhead"))
        .small()
        .enabled(can)
        .tooltip(tr!("從播放點開始放大 3 秒（之後可以改時間）；畫面平順地拉近再拉回", "Zooms in for 3 seconds from the playhead (you can change the time later), easing in and out"))
        .show(ui)
        .clicked()
    {
        ed.zoom_pick = true;
        ed.tool = None;
        ed.ann_sel = None;
    }
    let mut remove = None;
    let mut seek = None;
    let d = ed.duration;
    for (i, r) in ed.spec.zoom_regions.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(RichText::new(trf!("{}×", "{}×", speed_label((r.factor() * 10.0).round() / 10.0))).font(theme::font_bold(12.5)));
            let mut a = r.start;
            let mut b = r.end;
            if ui.add(egui::DragValue::new(&mut a).speed(0.05).range(0.0..=d).custom_formatter(|v, _| video_clock(v))).on_hover_text(tr!("開始（拖曳調整）", "Start (drag to adjust)")).changed()
            {
                r.start = a.min(r.end - 0.3);
            }
            ui.label("–");
            if ui.add(egui::DragValue::new(&mut b).speed(0.05).range(0.0..=d).custom_formatter(|v, _| video_clock(v))).on_hover_text(tr!("結束（拖曳調整）", "End (drag to adjust)")).changed()
            {
                r.end = b.max(r.start + 0.3);
            }
            if Btn::new(tr!("看看", "Show")).ghost().small().tooltip(tr!("跳到這裡", "Jump here")).show(ui).clicked() {
                seek = Some(r.start);
            }
            if Btn::new("×").ghost().small().tooltip(tr!("拿掉這個放大", "Remove this zoom")).show(ui).clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        ed.spec.zoom_regions.remove(i);
    }
    if let Some(t) = seek {
        ed.seek(t);
    }
    if !ed.spec.zoom_regions.is_empty() {
        hint(
            ui,
            tr!(
                "按「預覽結果」可以看到效果。和跟著點擊放大一起用時，手動放大的前後不放大點擊。",
                "Press “Preview result” to see it. Used with Zoom on clicks, clicks near a manual zoom aren't zoomed."
            ),
        );
    }
}

fn zoom_panel(ed: &mut Editor, ui: &mut egui::Ui) {
    sub_title(ui, tr!("跟著點擊放大", "Zoom on clicks"));
    if ed.clicks.is_empty() && ed.zooms.is_empty() {
        return hint(
            ui,
            tr!("這支錄影沒有記下滑鼠點擊（3.1 版以後在 Windows 上錄的影片才有）。", "This recording has no saved mouse clicks (only videos recorded on Windows with version 3.1 or later have them)."),
        );
    }
    let can = !ed.crop_on && ed.vertical == 0;
    let mut on = ed.zoom > 1.0;
    let n = ed.clicks.len();
    let label = if !ed.zooms.is_empty() {
        tr!("放大", "Zoom in").to_string()
    } else if screenrecorder_core::i18n::is_en() {
        format!("Zoom in on clicks ({n} click{})", if n == 1 { "" } else { "s" })
    } else {
        format!("點擊時放大（{n} 次點擊）")
    };
    if switch(ui, &mut on, label, can).changed() {
        ed.zoom = if on { 2.0 } else { 0.0 };
    }
    if on {
        let mut z = (ed.zoom * 10.0).round() as u32;
        let items: Vec<(u32, String)> = zoom::FACTORS.iter().map(|f| ((f * 10.0).round() as u32, format!("{f}×"))).collect();
        let refs: Vec<(u32, &str)> = items.iter().map(|(v, t)| (*v, t.as_str())).collect();
        if segmented(ui, &mut z, &refs, can) {
            ed.zoom = z as f64 / 10.0;
        }
        // 錄影時按快速鍵標記了要放大的地方：預設只放大那些，也可以改成每次點擊
        if !ed.zooms.is_empty() {
            let m = ed.zooms.len();
            let marked = trf!("標記的地方（{m}）", "Marked spots ({m})");
            let all = trf!("每次點擊（{n}）", "Every click ({n})");
            let mut v = ed.zoom_all_clicks;
            let items = if n > 0 { vec![(false, marked.as_str()), (true, all.as_str())] } else { vec![(false, marked.as_str())] };
            if segmented(ui, &mut v, &items, can) {
                ed.zoom_all_clicks = v;
            }
        }
    }
    if can && !ed.zooms.is_empty() && !ed.zoom_all_clicks {
        hint(
            ui,
            tr!(
                "只在錄影時按快速鍵（預設 Ctrl+Alt+Z）標記的地方慢慢放大，停一下再拉回全畫面。時間軸上的藍點是標記的地方，點一下可以設定這裡不放大；按「預覽結果」可以看到效果。",
                "Zooms in only on the spots you marked with the shortcut while recording (Ctrl+Alt+Z by default), then zooms back out. The blue dots on the timeline are the marked spots; click one to skip it. Press “Preview result” to see the effect."
            ),
        );
    } else if can {
        hint(
            ui,
            tr!(
                "點擊前畫面慢慢放大到點擊的地方，連續點擊時跟著移動，停下來後拉回全畫面。時間軸上的藍點是點擊，點一下可以設定這次不放大；按「預覽結果」可以看到效果。",
                "Before each click the view slowly zooms in on it, follows along during a series of clicks, then zooms back out. The blue dots on the timeline are clicks; click one to skip zooming on it. Press “Preview result” to see the effect."
            ),
        );
    } else if ed.vertical != 0 {
        warn_hint(ui, tr!("直式影片時不能使用（要用的話先把直式影片改成「不用」）。", "Can't be used with vertical video (set Vertical video to “Off” first)."));
    } else {
        warn_hint(ui, tr!("裁切畫面時不能使用（要用的話先按「取消裁切」）。", "Can't be used while the video is cropped (press “Remove crop” first)."));
    }
}

/// 直式影片（9:16）：放在中間，或跟著游標裁切
fn vertical_panel(ed: &mut Editor, ui: &mut egui::Ui) {
    let can_follow = !ed.cursor.is_empty() || !ed.clicks.is_empty();
    let mut v = ed.vertical;
    let items = [(0u8, tr!("不用", "Off")), (1, tr!("放在中間", "Fit")), (2, tr!("跟著游標", "Follow cursor"))];
    if segmented(ui, &mut v, &items, ed.vw > 0.0) {
        if v == 2 && !can_follow {
            v = ed.vertical;
        }
        ed.vertical = v;
        ed.vertical_pan.clear();
    }
    hint(
        ui,
        match ed.vertical {
            1 => tr!(
                "整個畫面放在直式畫面中間，上下是放大、模糊的同一個畫面，什麼都不會被裁掉。適合 Shorts、Reels、TikTok。",
                "The whole frame sits in the middle of a vertical frame, with a blurred, enlarged copy above and below, so nothing is cropped. Great for Shorts, Reels and TikTok."
            ),
            2 => tr!(
                "裁出 9:16 的一塊，像攝影機一樣跟著游標左右移動（游標在中間附近移動時不動）。不另外裁切畫面。",
                "Crops a 9:16 window that pans like a camera to follow the cursor (it stays still while the cursor moves near the middle). Manual cropping isn't applied."
            ),
            _ if !can_follow => tr!(
                "「跟著游標」需要錄影時記下的游標位置或點擊（3.1 版以後在 Windows 上錄的影片才有）。",
                "“Follow cursor” needs the cursor positions or clicks saved while recording (only videos recorded on Windows with version 3.1 or later have them)."
            ),
            _ => tr!("做成手機直式影片：放在中間，或跟著游標裁切。", "Make a vertical video for phones: fit the whole frame, or crop and follow the cursor."),
        },
    );
}

/// 平滑、放大的游標：錄影時記下的游標位置，輸出時畫成大的箭頭、平順地移動
fn cursor_panel(ed: &mut Editor, ui: &mut egui::Ui) {
    let p = theme::pal(ui);
    sub_title(ui, tr!("平滑、放大的游標", "Smooth, larger cursor"));
    if ed.cursor.is_empty() {
        return hint(
            ui,
            tr!(
                "這支錄影沒有記下游標的位置（3.1 版以後在 Windows 上錄的影片才有）。",
                "This recording has no saved cursor positions (only videos recorded on Windows with version 3.1 or later have them)."
            ),
        );
    }
    let mut on = ed.cursor_size > 0.0;
    if switch(ui, &mut on, tr!("換成大游標、平順移動", "Larger cursor that moves smoothly"), true).changed() {
        ed.cursor_size = if on { 2.0 } else { 0.0 };
    }
    if on {
        let mut v = (ed.cursor_size * 10.0).round() as u32;
        let items: Vec<(u32, String)> = screenrecorder_core::cursor::SIZES.iter().map(|f| ((f * 10.0).round() as u32, format!("{f}×"))).collect();
        let refs: Vec<(u32, &str)> = items.iter().map(|(v, t)| (*v, t.as_str())).collect();
        if segmented(ui, &mut v, &refs, true) {
            ed.cursor_size = v as f64 / 10.0;
        }
        theme::check(ui, &mut ed.cursor_hide, RichText::new(tr!("停著不動時隱藏", "Hide when idle")).font(theme::font(12.5))).on_hover_text(trf!(
            "滑鼠停著不動超過 {} 秒就把游標藏起來，一動就出現，畫面更乾淨",
            "Hide the cursor when the mouse hasn't moved for {} s; it reappears as soon as it moves",
            screenrecorder_core::cursor::IDLE
        ));
    }
    if ed.no_cursor {
        hint(
            ui,
            tr!(
                "游標像彈簧一樣平順地跟著移動，手抖、急停都變得流暢；畫在標註底下。",
                "The cursor follows smoothly like a spring, so shaky or abrupt movements look fluid. It's drawn below annotations."
            ),
        );
    } else {
        ui.label(
            RichText::new(tr!(
                "這支錄影有把游標錄進影片，會同時看到原本的小游標。下次錄影前到「設定 → 錄影」關掉「滑鼠游標：錄進影片」。",
                "This recording includes the cursor, so the original small cursor will also be visible. Before your next recording, turn off “Mouse cursor: Include in recording” in Settings → Recording."
            ))
            .font(theme::font(12.0))
            .color(p.warn),
        );
    }
}

/// 加上標註：(工具, 中文名稱, 英文名稱)
const MARK_TOOLS: [(Tool, &str, &str); 8] = [
    (Tool::Ann(AnnKind::Text), "文字", "Text"),
    (Tool::Emoji, "表情符號", "Emoji"),
    (Tool::Ann(AnnKind::Arrow), "箭頭", "Arrow"),
    (Tool::Ann(AnnKind::Rect), "方框", "Rectangle"),
    (Tool::Ann(AnnKind::Ellipse), "圓框", "Ellipse"),
    (Tool::Ann(AnnKind::Highlight), "螢光筆", "Highlighter"),
    (Tool::Ann(AnnKind::Pen), "畫筆", "Pen"),
    (Tool::Ann(AnnKind::Step), "編號", "Number"),
];
/// 遮蔽與強調
const COVER_TOOLS: [(Tool, &str, &str); 4] = [
    (Tool::Ann(AnnKind::Mosaic), "馬賽克", "Pixelate"),
    (Tool::Ann(AnnKind::Blur), "模糊", "Blur"),
    (Tool::Ann(AnnKind::Spotlight), "聚光燈", "Spotlight"),
    (Tool::Ann(AnnKind::Magnify), "放大鏡", "Magnifier"),
];

/// 一組工具：每列三個方塊；picture = 最後放一個「圖片」方塊（按下去選圖片來源）
fn tool_grid(ed: &mut Editor, ui: &mut egui::Ui, tools: &[(Tool, &str, &str)], can: bool, picture: bool) {
    let p = theme::pal(ui);
    let gap = 6.0;
    let tw = ((ui.available_width() - gap * 2.0) / 3.0).floor();
    // 放大鏡只能用在截圖
    let shot = ed.is_shot();
    let tools: Vec<(Tool, &str)> = tools.iter().filter(|(t, _, _)| shot || !t.kind().image_only()).map(|&(t, zh, en)| (t, tr!(zh, en))).collect();
    let rows = tools.len().div_ceil(3);
    for (ri, row) in tools.chunks(3).enumerate() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for &(tool, label) in row {
                let on = ed.tool == Some(tool);
                let (r, resp) = ui.allocate_exact_size(vec2(tw, 52.0), if can { Sense::click() } else { Sense::hover() });
                let hover = can && resp.hovered();
                let (fill, stroke, ink) = if on { (p.accent, p.accent, p.accent_ink) } else { (p.surface, if hover { p.accent } else { p.border }, p.text) };
                let ink = if can { ink } else { ink.gamma_multiply(0.45) };
                ui.painter().rect(r, CornerRadius::same(theme::RADIUS_SM), fill, Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
                let icon_color = if !on && tool == Tool::Ann(AnnKind::Highlight) { Color32::from_rgb(0xf5, 0xb3, 0x01) } else { ink };
                paint_tool_icon(
                    ui.painter(),
                    pos2(r.center().x, r.top() + 19.0),
                    tool,
                    if on && tool == Tool::Ann(AnnKind::Highlight) { Color32::WHITE } else { icon_color },
                    ed.emoji_tex.get(ed.emoji),
                );
                ui.painter().text(pos2(r.center().x, r.bottom() - 11.0), Align2::CENTER_CENTER, label, theme::font(12.0), ink);
                // 截圖：提示顯示快速鍵「箭頭（A）」
                let resp = match super::shot::tool_tip(tool, label).filter(|_| shot) {
                    Some(t) => resp.on_hover_text(t),
                    None => resp,
                };
                if can && resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    ed.tool = if on { None } else { Some(tool) };
                    if ed.tool.is_some() {
                        ed.ann_sel = None;
                    }
                }
            }
            if picture && ri + 1 == rows && row.len() < 3 {
                let (r, resp) = ui.allocate_exact_size(vec2(tw, 52.0), if can { Sense::click() } else { Sense::hover() });
                let ink = if can { p.text } else { p.text.gamma_multiply(0.45) };
                let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&resp));
                ui.painter().rect(r, CornerRadius::same(theme::RADIUS_SM), p.surface, Stroke::new(1.0, if (can && resp.hovered()) || open { p.accent } else { p.border }), egui::StrokeKind::Inside);
                theme::paint_icon(ui.painter(), egui::Rect::from_center_size(pos2(r.center().x, r.top() + 19.0), vec2(18.0, 18.0)), Icon::Image, ink);
                ui.painter().text(pos2(r.center().x, r.bottom() - 11.0), Align2::CENTER_CENTER, tr!("圖片", "Image"), theme::font(12.0), ink);
                if can {
                    let more = if shot { tr!("", " and opacity") } else { tr!("、出現時間", ", opacity and timing") };
                    let tip = trf!(
                        "加上圖片：Logo、浮水印、商品照…（可以調整大小、透明度{}）\n也可以直接把圖片檔拖曳到這個視窗",
                        "Add an image: a logo, watermark, product photo… (you can adjust the size{})\nYou can also drag an image file into this window",
                        more
                    );
                    let resp = resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tip);
                    picture_menu(ed, &resp);
                }
            }
        });
    }
}

fn ann_panel(ed: &mut Editor, ui: &mut egui::Ui, ctx: &egui::Context) {
    let can = ed.vw > 0.0;
    let shot = ed.is_shot();

    // 選取了標註（沒有在放新的）：工具收成一排小圖示，屬性（字級、角度、出現時間）才不會被擠到下面看不到
    if ed.tool.is_none() && ed.selected().is_some() {
        tool_row(ed, ui, can);
        props(ed, ui, ctx);
        return ann_list(ed, ui);
    }

    // 加上標註：文字、箭頭、框線…、圖片
    section(ui, tr!("加上標註", "Add annotations"), true);
    tool_grid(ed, ui, &MARK_TOOLS, can, true);
    if ed.tool == Some(Tool::Emoji) {
        if let Some(i) = emoji_picker(ed, ui, None) {
            ed.emoji = i;
        }
    }
    // 遮蔽與強調：馬賽克、模糊、聚光燈、放大鏡；截圖可以自動遮個資
    section(ui, tr!("遮蔽與強調", "Hide and emphasize"), false);
    tool_grid(ed, ui, &COVER_TOOLS, can, false);
    if shot && can {
        let busy = ed.finding_pii;
        let label = if busy { tr!("正在找個資…", "Finding personal info…") } else { tr!("自動遮個資", "Auto-redact") };
        let resp = Btn::new(label)
            .small()
            .min_width(ui.available_width())
            .enabled(!busy)
            .tooltip(tr!(
                "用文字辨識找出圖裡的 Email、電話、身分證字號、信用卡號，自動打上馬賽克（可再個別調整或刪除）",
                "Use text recognition to find emails, phone numbers, ID numbers and credit card numbers in the image and pixelate them automatically (you can adjust or delete each one afterward)"
            ))
            .show(ui);
        if resp.clicked() {
            ed.pending = Some(super::shot::Act::FindPii);
        }
    }
    let w = ed.what();
    let hint_text = if !can {
        trf!("無法讀取{w}尺寸，不能加上標註。", "Couldn't read the {w} size, so annotations can't be added.")
    } else {
        match ed.tool {
            // 提示短一點，句子不要在「復／原」、「）」之間斷行
            Some(Tool::Ann(AnnKind::Step)) => trf!("在{w}上依序點一下，放上編號 1、2、3…\n按 Esc 結束", "Click on the {w} in order to place 1, 2, 3…\nPress Esc when done"),
            Some(Tool::Emoji) => trf!("選好表情符號，在{w}上點一下放置，可連續放\n按 Esc 結束", "Pick an emoji, then click on the {w} to place it (repeatable)\nPress Esc when done"),
            Some(Tool::Ann(AnnKind::Text)) => trf!("在{w}上點一下放置文字。按 Esc 取消", "Click on the {w} to place it. Esc cancels"),
            Some(Tool::Ann(AnnKind::Pen)) => trf!("在{w}上按住拖曳手繪，可以畫好幾筆\n按 Esc 結束", "Drag on the {w} to draw (several strokes are fine)\nPress Esc when done"),
            Some(Tool::Ann(AnnKind::Magnify)) => trf!("在{w}上拖曳框出要放大的地方。按 Esc 取消", "Drag on the {w} over the area to magnify. Esc cancels"),
            Some(_) => trf!("在{w}上按住拖曳放置。按 Esc 取消", "Drag on the {w} to place it. Esc cancels"),
            None if ed.selected().is_some() => String::new(),
            None if shot => tr!("選工具後在圖上點一下或拖曳。Ctrl+Z 復原", "Pick a tool, then click or drag on the image. Ctrl+Z undoes").into(),
            None => tr!(
                "① 選工具 ② 在影片上點一下或拖曳放置。標註從目前位置起出現 3 秒，可在時間軸下方的標註軌拖曳調整。",
                "① Pick a tool ② Click or drag on the video to place it. Annotations show for 3 s from the current position; drag them on the annotation track below the timeline to adjust."
            )
            .into(),
        }
    };
    if !hint_text.is_empty() {
        hint(ui, &hint_text);
    }
    tool_style(ed, ui);
    ann_list(ed, ui);
}

/// 選取標註時的工具列：一排小圖示（點一下換成放新的標註）
fn tool_row(ed: &mut Editor, ui: &mut egui::Ui, can: bool) {
    let p = theme::pal(ui);
    let shot = ed.is_shot();
    let tools: Vec<(Tool, &str)> = MARK_TOOLS.iter().chain(COVER_TOOLS.iter()).filter(|(t, _, _)| shot || !t.kind().image_only()).map(|&(t, zh, en)| (t, tr!(zh, en))).collect();
    let gap = 3.0;
    let n = tools.len() as f32 + 1.0;
    let size = ((ui.available_width() - gap * (n - 1.0)) / n).floor().clamp(20.0, 32.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(gap, gap);
        for (tool, label) in tools {
            let (r, resp) = ui.allocate_exact_size(vec2(size, size), if can { Sense::click() } else { Sense::hover() });
            let hover = can && resp.hovered();
            ui.painter().rect(r, CornerRadius::same(5), p.surface, Stroke::new(1.0, if hover { p.accent } else { p.border }), egui::StrokeKind::Inside);
            let ink = if can { p.text } else { p.text.gamma_multiply(0.45) };
            // 圖示畫小一點才放得進小方塊
            let painter = ui.painter().with_clip_rect(r);
            paint_tool_icon(&painter, r.center(), tool, if tool == Tool::Ann(AnnKind::Highlight) { Color32::from_rgb(0xf5, 0xb3, 0x01) } else { ink }, ed.emoji_tex.get(ed.emoji));
            if resp.on_hover_text(trf!("加上{label}", "Add {label}")).on_hover_cursor(CursorIcon::PointingHand).clicked() && can {
                ed.tool = Some(tool);
                ed.ann_sel = None;
            }
        }
        let (r, resp) = ui.allocate_exact_size(vec2(size, size), if can { Sense::click() } else { Sense::hover() });
        ui.painter().rect(r, CornerRadius::same(5), p.surface, Stroke::new(1.0, if can && resp.hovered() { p.accent } else { p.border }), egui::StrokeKind::Inside);
        theme::paint_icon(ui.painter(), egui::Rect::from_center_size(r.center(), vec2(16.0, 16.0)), Icon::Image, p.text);
        if can {
            let resp = resp.on_hover_text(tr!("加上圖片（Logo、浮水印…）", "Add an image (logo, watermark…)")).on_hover_cursor(CursorIcon::PointingHand);
            picture_menu(ed, &resp);
        }
    });
}

/// 加上圖片 / Logo：選檔、上次用的圖、剪貼簿的圖（也可以直接把圖片檔拖曳進來）
fn picture_menu(ed: &mut Editor, resp: &egui::Response) {
    egui::Popup::menu(resp).show(|ui| {
        ui.set_min_width(220.0);
        if ui.button(tr!("選擇圖片檔…", "Choose image file…")).clicked() {
            ed.pending = Some(Act::PickPicture { replace: false });
        }
        // 選單開著時才檢查檔案還在不在（不要每一格畫面都讀磁碟）
        let last = ed.last_picture.clone();
        if !last.is_empty() && std::path::Path::new(&last).is_file() && ui.button(trf!("上次的圖片「{}」", "Last image “{}”", file_name(&last))).on_hover_text(last.as_str()).clicked() {
            ed.pending = Some(Act::LastPicture);
        }
        if ui.button(tr!("貼上剪貼簿裡的圖片", "Paste image from clipboard")).clicked() {
            ed.pending = Some(Act::PastePicture);
        }
    });
}

/// 顏色與大小（線寬、粗細、字級）；回傳 (顏色改了, 大小改了)
fn style_controls(ui: &mut egui::Ui, kind: AnnKind, color: &mut String, size: &mut f64, k: f64, mine: &mut Vec<annotate::AnnStyle>) -> (bool, bool) {
    let p = theme::pal(ui);
    let (mut cc, mut sc) = (false, false);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        for c in COLORS {
            let (r, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
            let (cr, cg, cb) = annotate::parse_color(c);
            if color == c {
                ui.painter().circle_stroke(r.center(), 13.5, Stroke::new(2.0, p.accent));
            }
            ui.painter().circle(r.center(), 10.5, Color32::from_rgb(cr, cg, cb), Stroke::new(1.0, p.border_strong));
            if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(trf!("顏色 {c}", "Color {c}")).clicked() && color != c {
                *color = c.to_string();
                cc = true;
            }
        }
    });
    let label = match kind {
        AnnKind::Text => tr!("字級", "Font size"),
        AnnKind::Step => tr!("大小", "Size"),
        AnnKind::Pen => tr!("粗細", "Thickness"),
        _ => tr!("線寬", "Line width"),
    };
    let (lo, hi) = match kind {
        AnnKind::Text => (16.0, 240.0),
        AnnKind::Step => (24.0, 240.0),
        _ => (2.0, 40.0),
    };
    ui.horizontal(|ui| {
        ui.label(theme::muted(ui, label).font(theme::font(12.0)));
        let mut v = *size;
        ui.spacing_mut().slider_width = ui.available_width() - 50.0;
        theme::slider_style(ui);
        if ui.add(egui::Slider::new(&mut v, (lo * k).round()..=(hi * k).round()).step_by(1.0).fixed_decimals(0)).changed() {
            *size = v;
            sc = true;
        }
    });
    // 我的樣式：點一下套用；按右鍵刪除；「存起來」把目前的顏色與大小加進去
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        ui.label(theme::muted(ui, tr!("我的樣式", "My styles")).font(theme::font(12.0)));
        let mut remove = None;
        for (i, st) in mine.iter().enumerate() {
            let label = format!("{}", st.size.round());
            let g = ui.painter().layout_no_wrap(label, theme::font(11.5), p.text);
            let (r, resp) = ui.allocate_exact_size(vec2(g.size().x + 30.0, 24.0), Sense::click());
            let on = color.eq_ignore_ascii_case(&st.color) && (*size - (st.size * k).round()).abs() < 0.5;
            ui.painter().rect(r, CornerRadius::same(12), p.surface, Stroke::new(if on { 2.0 } else { 1.0 }, if on { p.accent } else { p.border }), egui::StrokeKind::Inside);
            let (cr, cg, cb) = annotate::parse_color(&st.color);
            ui.painter().circle(pos2(r.left() + 12.0, r.center().y), 7.0, Color32::from_rgb(cr, cg, cb), Stroke::new(1.0, p.border_strong));
            ui.painter().galley(pos2(r.left() + 23.0, r.center().y - g.size().y / 2.0), g, p.text);
            let resp = resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tr!("套用這個樣式（按右鍵刪除）", "Apply this style (right-click to delete)"));
            if resp.clicked() {
                let (lo_s, hi_s) = ((lo * k).round(), (hi * k).round());
                *color = st.color.clone();
                *size = (st.size * k).round().clamp(lo_s, hi_s);
                cc = true;
                sc = true;
            }
            resp.context_menu(|ui| {
                if ui.button(tr!("刪除這個樣式", "Delete this style")).clicked() {
                    remove = Some(i);
                    ui.close();
                }
            });
        }
        if let Some(i) = remove {
            mine.remove(i);
        }
        if Btn::new(tr!("＋ 存起來", "+ Save"))
            .small()
            .ghost()
            .tooltip(tr!("把目前的顏色與大小存成我的樣式，截圖和剪輯都可以用", "Save the current color and size as one of your styles for screenshots and videos"))
            .show(ui)
            .clicked()
        {
            annotate::save_style(mine, color, *size / k.max(1e-6));
        }
    });
    (cc, sc)
}

/// 選了工具、還沒放：接下來放的標註用的顏色與大小
fn tool_style(ed: &mut Editor, ui: &mut egui::Ui) {
    let Some(tool) = ed.tool else { return };
    let kind = tool.kind();
    if tool == Tool::Emoji || kind.is_effect() || kind.shape_only() || ed.selected().is_some() {
        return;
    }
    let p = theme::pal(ui);
    let vh = ed.size_base();
    let (mut color, mut size) = super::style_of(kind, vh);
    egui::Frame::new().fill(p.surface2).corner_radius(theme::RADIUS_SM).inner_margin(10).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
        ui.label(
            RichText::new(trf!(
                "{}的顏色與{}",
                "{} color and {}",
                kind.label(),
                if kind == AnnKind::Pen {
                    tr!("粗細", "thickness")
                } else if kind == AnnKind::Text {
                    tr!("字級", "font size")
                } else if kind == AnnKind::Step {
                    tr!("大小", "size")
                } else {
                    tr!("線寬", "line width")
                }
            ))
            .font(theme::font_bold(13.0)),
        );
        let (cc, sc) = style_controls(ui, kind, &mut color, &mut size, vh / 1080.0, &mut ed.ann_styles);
        if cc || sc {
            super::remember_style(kind, &color, size, vh);
        }
        ui.label(theme::muted(ui, tr!("接下來放的都用這個設定；放好的可以點選後再改。", "New annotations use these settings; select a placed one to change it.")).font(theme::font(12.0)));
    });
}

/// 選取的標註的屬性（改一份副本，最後寫回）；字幕分頁也用（字幕的文字在清單裡改）
pub(super) fn props(ed: &mut Editor, ui: &mut egui::Ui, ctx: &egui::Context) {
    let p = theme::pal(ui);
    let keep = ed.keep();
    let Some(a) = ed.selected().cloned() else {
        return;
    };
    let mut cur = a.clone();
    let (dur, now) = (ed.duration, ed.now());
    let k = ed.size_base() / 1080.0;
    let sub = ed.is_sub(a.id);
    let emoji = cur.kind == AnnKind::Text && cur.text.as_deref().is_some_and(super::is_emoji_text);
    let mut delete = false;
    let mut apply_all: Vec<u64> = vec![];
    egui::Frame::new().fill(p.surface2).corner_radius(theme::RADIUS_SM).inner_margin(10).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
        ui.horizontal(|ui| {
            let title = if sub { trf!("字幕 {}", "Subtitle {}", annotate::label(&a)) } else { annotate::label(&a) };
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if Btn::new(tr!("刪除", "Delete")).danger().small().tooltip(tr!("快速鍵 Delete", "Shortcut: Delete")).show(ui).clicked() {
                    delete = true;
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| ui.add(egui::Label::new(RichText::new(title).font(theme::font_bold(13.5))).truncate()));
            });
        });
        if !Editor::in_output(&a, &keep) {
            egui::Frame::new().fill(p.warn_soft).corner_radius(6).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new(tr!(
                        "這個標註在刪除的片段中，輸出的影片看不到。請在標註軌把它拖到保留的部分。",
                        "This annotation is in a removed section and won't appear in the output video. Drag it to a kept part on the annotation track."
                    ))
                    .font(theme::font(12.0))
                    .color(p.warn),
                );
            });
        }
        // 出現方式（影片）：淡入淡出、放大出現；馬賽克、模糊不能
        if !ed.is_shot() && !cur.kind.is_effect() {
            ui.horizontal(|ui| {
                ui.label(theme::muted(ui, tr!("出現方式", "Appear")).font(theme::font(12.0)));
                let items = [(0u8, tr!("直接", "Instant")), (1, tr!("淡入淡出", "Fade")), (2, tr!("放大出現", "Pop"))];
                segmented(ui, &mut cur.anim, &items, true);
            });
        }
        let is_text = cur.kind == AnnKind::Text && !emoji && !sub;
        if is_text {
            ui.label(theme::muted(ui, tr!("文字（Enter 換行）", "Text (Enter for a new line)")).font(theme::font(12.0)));
            let mut text = cur.text.clone().unwrap_or_default();
            let out = egui::TextEdit::multiline(&mut text).desired_rows(2).desired_width(f32::INFINITY).font(theme::font(14.0)).show(ui);
            if out.response.changed() {
                cur.text = Some(text.clone());
            }
            // 放開滑鼠後才移過去（按下那一格移過去，放開時會被取消）
            let pointer_busy = ui.input(|i| i.pointer.any_down() || i.pointer.any_released());
            if ed.focus_text && !pointer_busy {
                ed.focus_text = false;
                // 新加的文字：移到文字欄並全選，直接打字取代
                out.response.request_focus();
                let mut state = out.state;
                let n = text.chars().count();
                state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(n))));
                state.store(ctx, out.response.id);
            }
        }
        if emoji {
            // 表情符號：換一個、調整大小（沒有文字、顏色、深色底）
            ui.label(theme::muted(ui, tr!("換一個表情符號", "Change the emoji")).font(theme::font(12.0)));
            if let Some(i) = emoji_picker(ed, ui, cur.text.as_deref()) {
                cur.text = Some(screenrecorder_core::annotate::EMOJIS[i].to_string());
                ed.emoji = i;
            }
            ui.horizontal(|ui| {
                ui.label(theme::muted(ui, tr!("大小", "Size")).font(theme::font(12.0)));
                let mut v = cur.size;
                ui.spacing_mut().slider_width = ui.available_width() - 50.0;
                theme::slider_style(ui);
                if ui.add(egui::Slider::new(&mut v, (24.0 * k).round()..=(400.0 * k).round()).step_by(1.0).fixed_decimals(0)).changed() {
                    let (cx, cy) = (cur.x + cur.w / 2.0, cur.y + cur.h / 2.0);
                    cur.size = v;
                    annotate::measure(&mut cur);
                    cur.x = cx - cur.w / 2.0;
                    cur.y = cy - cur.h / 2.0;
                }
            });
        } else if cur.kind == AnnKind::Magnify {
            ui.horizontal(|ui| {
                ui.label(theme::muted(ui, tr!("倍率", "Zoom")).font(theme::font(12.0)));
                let mut z = cur.size / 100.0;
                ui.spacing_mut().slider_width = ui.available_width() - 60.0;
                theme::slider_style(ui);
                if ui.add(egui::Slider::new(&mut z, 1.5..=4.0).step_by(0.25).fixed_decimals(2).suffix("×")).changed() {
                    cur.size = (z * 100.0).round();
                }
            });
        } else if cur.kind == AnnKind::Image {
            ui.horizontal(|ui| {
                ui.label(theme::muted(ui, tr!("不透明度", "Opacity")).font(theme::font(12.0)));
                let mut v = cur.size.clamp(5.0, 100.0);
                ui.spacing_mut().slider_width = ui.available_width() - 60.0;
                theme::slider_style(ui);
                if ui.add(egui::Slider::new(&mut v, 5.0..=100.0).step_by(5.0).fixed_decimals(0).suffix("%")).changed() {
                    cur.size = v;
                }
            });
            ui.horizontal(|ui| {
                if Btn::new(tr!("換一張圖…", "Change image…")).ghost().small().show(ui).clicked() {
                    ed.pending = Some(Act::PickPicture { replace: true });
                }
                if Btn::new(tr!("原始比例", "Original ratio")).ghost().small().tooltip(tr!("依圖片原本的長寬比例調整高度", "Adjust the height to the image's original aspect ratio")).show(ui).clicked()
                {
                    if let Some(pic) = cur.text.as_deref().and_then(picture::load) {
                        let cy = cur.y + cur.h / 2.0;
                        cur.h = (cur.w * pic.height() as f64 / pic.width().max(1) as f64).round().max(8.0);
                        cur.y = cy - cur.h / 2.0;
                    }
                }
            });
            if cur.text.as_deref().and_then(picture::load).is_none() {
                hint(ui, tr!("找不到這張圖片（可能被移動或刪除了），請按「換一張圖」。", "Can't find this image (it may have been moved or deleted). Press “Change image”."));
            } else {
                hint(ui, tr!("拖曳右下角調整大小（保持比例）；調低不透明度可以當作浮水印。", "Drag the bottom-right corner to resize (keeps the ratio); lower the opacity to use it as a watermark."));
            }
        } else if cur.kind.shape_only() {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.label(theme::muted(ui, tr!("形狀", "Shape")).font(theme::font(12.0)));
                let mut shape = cur.shape.unwrap_or(Shape::Round);
                let items: Vec<(Shape, &str)> = Shape::ALL.iter().map(|s| (*s, s.label())).collect();
                if segmented(ui, &mut shape, &items, true) {
                    cur.shape = Some(shape);
                }
            });
            hint(ui, tr!("框以外的地方會變暗，凸顯框裡的重點。", "Everything outside the shape is darkened to bring out what's inside."));
        } else if cur.kind.is_effect() {
            let idx = (cur.kind == AnnKind::Blur) as usize;
            let word = if cur.kind == AnnKind::Mosaic { tr!("馬賽克", "Pixelate") } else { tr!("模糊", "Blur") };
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.label(theme::muted(ui, tr!("形狀", "Shape")).font(theme::font(12.0)));
                let mut shape = cur.shape.unwrap_or_default();
                let items: Vec<(Shape, &str)> = Shape::ALL.iter().map(|s| (*s, s.label())).collect();
                if segmented(ui, &mut shape, &items, true) {
                    cur.shape = Some(shape);
                    ed.last_style[idx].0 = shape;
                }
            });
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.label(theme::muted(ui, tr!("範圍", "Area")).font(theme::font(12.0)));
                let mut inv = cur.invert;
                let (l0, l1) = (trf!("框內{word}", "{word} inside"), trf!("框外{word}（框內清楚）", "{word} outside"));
                if segmented(ui, &mut inv, &[(false, l0.as_str()), (true, l1.as_str())], true) {
                    cur.invert = inv;
                    ed.last_style[idx].1 = inv;
                }
            });
        } else {
            let (cc, sc) = style_controls(ui, cur.kind, &mut cur.color, &mut cur.size, k, &mut ed.ann_styles);
            if sc {
                // 文字、編號放大時以中心為準（字幕以下緣為準，才不會往上跑）
                let (cx, cy, bottom) = (cur.x + cur.w / 2.0, cur.y + cur.h / 2.0, cur.y + cur.h);
                annotate::measure(&mut cur);
                if matches!(cur.kind, AnnKind::Text | AnnKind::Step) {
                    cur.x = cx - cur.w / 2.0;
                    cur.y = if sub { bottom - cur.h } else { cy - cur.h / 2.0 };
                }
            }
            if (cc || sc) && !sub {
                // 之後新放的同類標註沿用（字幕的樣式不影響一般的文字）
                super::remember_style(cur.kind, &cur.color, cur.size, ed.size_base());
            }
            if cur.kind == AnnKind::Text {
                switch(ui, &mut cur.bg, tr!("深色底", "Dark background"), true);
            }
        }

        // 同類的標註一次改成一樣（例如所有編號都改成綠色、一樣大）：字幕只改字幕，一般的文字不改到字幕；表情符號不算
        let same: Vec<u64> = if emoji {
            vec![]
        } else {
            ed.anns.iter().filter(|o| o.kind == cur.kind && ed.is_sub(o.id) == sub && !(o.kind == AnnKind::Text && o.text.as_deref().is_some_and(super::is_emoji_text))).map(|o| o.id).collect()
        };
        let n = same.len();
        let en = screenrecorder_core::i18n::is_en();
        let all = match (sub, en) {
            (true, false) => format!("全部 {n} 段字幕都改成這樣"),
            (true, true) => format!("Apply to all {n} subtitles"),
            (false, false) => format!("全部 {n} 個{}都改成這樣", cur.kind.label()),
            (false, true) => format!("Apply to all {n} {} annotations", cur.kind.label().to_lowercase()),
        };
        let tip = if sub {
            tr!("顏色、字級、深色底套用到所有字幕（一般的文字標註不會改）", "Apply the color, font size and background to all subtitles (ordinary text annotations aren't changed)")
        } else if cur.kind == AnnKind::Text {
            tr!("顏色、字級、深色底套用到所有文字標註（字幕不會改）", "Apply the color, font size and background to all text annotations (subtitles aren't changed)")
        } else {
            tr!("顏色、大小（以及形狀、底色）套用到所有同類的標註", "Apply the color and size (and shape and background) to all annotations of this type")
        };
        if n > 1 && Btn::new(all).ghost().small().tooltip(tip).show(ui).clicked() {
            apply_all = same;
        }
        // 箭頭：彎成弧線（拖中間的圓點，或用這裡的滑桿）
        if cur.kind == AnnKind::Arrow {
            ui.horizontal(|ui| {
                ui.label(theme::muted(ui, tr!("彎曲", "Curve")).font(theme::font(12.0)));
                let len = cur.w.hypot(cur.h).max(1.0);
                let mut pct = (cur.bend / len * 100.0).round();
                ui.spacing_mut().slider_width = ui.available_width() - 120.0;
                theme::slider_style(ui);
                if ui
                    .add(egui::Slider::new(&mut pct, -75.0..=75.0).step_by(1.0).fixed_decimals(0).suffix("%"))
                    .on_hover_text(tr!("也可以直接拖曳箭頭中間的圓點；左右是往哪一邊彎", "You can also drag the dot in the middle of the arrow; the sign picks which way it bends"))
                    .changed()
                {
                    cur.bend = (pct / 100.0 * len).round();
                }
                if Btn::new(tr!("拉直", "Straighten")).ghost().small().enabled(cur.bend != 0.0).show(ui).clicked() {
                    cur.bend = 0.0;
                }
            });
        }
        // 角度（箭頭不用：兩端本來就能指向任何方向；字幕不用）
        if cur.kind.rotatable() && !sub {
            ui.horizontal(|ui| {
                ui.label(theme::muted(ui, tr!("角度", "Angle")).font(theme::font(12.0)));
                let mut deg = cur.rot;
                ui.spacing_mut().slider_width = ui.available_width() - 120.0;
                theme::slider_style(ui);
                if ui
                    .add(egui::Slider::new(&mut deg, -180.0..=180.0).step_by(1.0).fixed_decimals(0).suffix("°"))
                    .on_hover_text(tr!(
                        "也可以拖曳影片上選取框旁的旋轉鈕，或按 [ / ] 每次轉 15 度（加 Shift 每次 1 度）",
                        "You can also drag the rotate handle next to the selection box, or press [ / ] to rotate 15° at a time (1° with Shift)"
                    ))
                    .changed()
                {
                    cur.rot = deg;
                }
                if Btn::new(tr!("歸零", "Reset")).ghost().small().enabled(cur.rot != 0.0).show(ui).clicked() {
                    cur.rot = 0.0;
                }
            });
        }

        // 出現時間（截圖沒有）：[−] 時間 [＋]，每次 0.1 秒，也可以直接打「1:05.5」或「65.5」
        if ed.is_shot() {
            return;
        }
        let mut bad = false;
        for i in 0..2 {
            ui.horizontal(|ui| {
                ui.add_sized(vec2(30.0, 20.0), egui::Label::new(theme::muted(ui, if i == 0 { tr!("出現", "From") } else { tr!("結束", "To") }).font(theme::font(12.0))));
                let id = Id::new(("ed-time", i));
                let typed: Option<String> = ui.data(|d| d.get_temp::<String>(id));
                let (mut v, range) = if i == 0 { (cur.start, 0.0..=(cur.end - 0.1).max(0.0)) } else { (cur.end, (cur.start + 0.1).min(dur)..=dur) };
                if theme::stepper_with(ui, id, &mut v, 0.1, range, video_clock, parse_time) {
                    if i == 0 {
                        cur.start = v;
                    } else {
                        cur.end = v;
                    }
                }
                let now_typed: Option<String> = ui.data(|d| d.get_temp::<String>(id));
                // 打字中看不懂、或剛離開時看不懂（恢復原值）：提示格式
                if now_typed.as_deref().is_some_and(|t| !t.trim().is_empty() && parse_time(t).is_none()) {
                    bad = true;
                }
                if let (Some(t), None) = (&typed, &now_typed) {
                    ed.time_bad = parse_time(t).is_none().then_some((a.id, i));
                }
                if ed.time_bad == Some((a.id, i)) {
                    bad = true;
                }
                let tip = if i == 0 { tr!("從播放頭的時間開始", "Start at the playhead position") } else { tr!("到播放頭的時間結束", "End at the playhead position") };
                if Btn::new(tr!("用目前時間", "Use playhead")).ghost().small().tooltip(tip).show(ui).clicked() {
                    if i == 0 {
                        cur.start = now.min(cur.end - 0.1).max(0.0);
                    } else {
                        cur.end = now.max(cur.start + 0.1).min(dur);
                    }
                    ed.time_bad = None;
                }
            });
        }
        if bad {
            ui.label(RichText::new(tr!("看不懂這個時間，格式例如 1:05.5 或 65.5", "Couldn't read that time. Use a format like 1:05.5 or 65.5")).font(theme::font(12.0)).color(p.rec));
        }
    });
    if delete {
        return ed.delete_ann(a.id);
    }
    if cur != a {
        if let Some(sel) = ed.selected_mut() {
            *sel = cur.clone();
        }
        ed.touch_selected();
    }
    for id in apply_all.into_iter().filter(|&id| id != cur.id) {
        let Some(o) = ed.ann_mut(id) else { continue };
        let (cx, cy, bottom) = (o.x + o.w / 2.0, o.y + o.h / 2.0, o.y + o.h);
        o.color = cur.color.clone();
        o.size = cur.size;
        o.bg = cur.bg;
        if o.kind.is_effect() {
            o.shape = cur.shape;
            o.invert = cur.invert;
        }
        annotate::measure(o);
        if matches!(o.kind, AnnKind::Text | AnnKind::Step) {
            o.x = cx - o.w / 2.0;
            o.y = if sub { bottom - o.h } else { cy - o.h / 2.0 };
        }
    }
}

/// 表情符號的選單（目前的框起來）；回傳點了哪一個
fn emoji_picker(ed: &Editor, ui: &mut egui::Ui, current: Option<&str>) -> Option<usize> {
    let p = theme::pal(ui);
    let mut picked = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
        for (i, t) in ed.emoji_tex.iter().enumerate() {
            let (r, resp) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::click());
            let on = current.map(|c| c == screenrecorder_core::annotate::EMOJIS[i]).unwrap_or(ed.emoji == i);
            if on || resp.hovered() {
                ui.painter().rect(r, CornerRadius::same(6), p.surface, if on { Stroke::new(2.0, p.accent) } else { Stroke::NONE }, egui::StrokeKind::Inside);
            }
            let sz = t.size_vec2();
            let k = 24.0 / sz.y.max(1.0);
            ui.painter().image(t.id(), egui::Rect::from_center_size(r.center(), sz * k), egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                picked = Some(i);
            }
        }
    });
    picked
}

/// 一行文字，放不下時結尾換成「…」
pub(super) fn ellipsized(painter: &egui::Painter, text: String, font: egui::FontId, color: Color32, max_w: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text, font, color);
    job.wrap = egui::text::TextWrapping { max_width: max_w.max(1.0), max_rows: 1, break_anywhere: true, overflow_character: Some('…') };
    painter.layout_job(job)
}

/// 標註清單（依出現時間；字幕在「字幕」分頁）：點一下選取並跳到它出現的時間
fn ann_list(ed: &mut Editor, ui: &mut egui::Ui) {
    let subs = ed.sub_ids.len();
    if ed.anns.len() <= subs {
        if subs > 0 && !ed.is_shot() {
            hint(ui, &trf!("字幕（{subs} 段）在「字幕」分頁修改。", "Subtitles ({subs}) are edited in the “Subtitles” tab."));
        }
        return;
    }
    let p = theme::pal(ui);
    let keep = ed.keep();
    let mut list: Vec<(u64, f64, f64, String, Color32, bool)> =
        ed.anns.iter().filter(|a| !ed.is_sub(a.id)).map(|a| (a.id, a.start, a.end, annotate::label(a), ann_color(a), !Editor::in_output(a, &keep))).collect();
    list.sort_by(|x, y| x.1.total_cmp(&y.1).then(x.0.cmp(&y.0)));
    let mut select = None;
    let mut delete = None;
    let shot = ed.is_shot();
    ui.spacing_mut().item_spacing.y = 4.0;
    for (id, start, end, name, color, gone) in list {
        let w = ui.available_width();
        let (r, resp) = ui.allocate_exact_size(vec2(w, 28.0), Sense::click());
        let on = ed.ann_sel == Some(id);
        let painter = ui.painter();
        painter.rect(r, CornerRadius::same(6), if on { p.accent.gamma_multiply(0.12) } else { Color32::TRANSPARENT }, Stroke::new(1.0, if on { p.accent } else { p.border }), egui::StrokeKind::Inside);
        painter.circle(pos2(r.left() + 13.0, r.center().y), 5.0, color, Stroke::new(1.0, Color32::from_black_alpha(77)));
        let times = painter.layout_no_wrap(if shot { String::new() } else { format!("{}–{}", video_clock(start), video_clock(end)) }, theme::mono(11.5), p.muted);
        let x_rect = egui::Rect::from_center_size(pos2(r.right() - 14.0, r.center().y), vec2(22.0, 22.0));
        let tx = x_rect.left() - 4.0 - times.size().x;
        let name_rect = egui::Rect::from_min_max(pos2(r.left() + 24.0, r.top()), pos2(tx - 6.0, r.bottom()));
        // 名稱太長時結尾變成「…」
        let ng = ellipsized(painter, name, theme::font(12.5), if gone { p.muted } else { p.text }, name_rect.width());
        let ny = r.center().y - ng.size().y / 2.0;
        let nw = ng.size().x.min(name_rect.width());
        painter.galley(pos2(name_rect.left(), ny), ng, p.text);
        if gone {
            painter.hline(name_rect.left()..=name_rect.left() + nw, r.center().y, Stroke::new(1.0, p.muted));
        }
        painter.galley(pos2(tx, r.center().y - times.size().y / 2.0), times, p.muted);
        let xr = ui.interact(x_rect, Id::new(("ed-ann-del", id)), Sense::click()).on_hover_text(tr!("刪除這個標註", "Delete this annotation")).on_hover_cursor(CursorIcon::PointingHand);
        if xr.hovered() {
            ui.painter().circle_filled(x_rect.center(), 11.0, p.rec_soft);
        }
        ui.painter().text(x_rect.center(), Align2::CENTER_CENTER, "×", theme::font(14.0), if xr.hovered() { p.rec } else { p.text });
        if xr.clicked() {
            delete = Some(id);
        } else if resp
            .on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text(if gone { tr!("在刪除的片段中，不會出現在輸出影片", "In a removed section; won't appear in the output video") } else { "" })
            .clicked()
        {
            select = Some((id, start, end));
        }
    }
    if subs > 0 && !shot {
        hint(ui, &trf!("字幕（{subs} 段）在「字幕」分頁修改。", "Subtitles ({subs}) are edited in the “Subtitles” tab."));
    }
    if let Some(id) = delete {
        ed.delete_ann(id);
    }
    if let Some((id, start, end)) = select {
        // 選取並跳到它出現的時間
        let now = ed.now();
        if now < start || now > end {
            ed.seek(start);
        }
        ed.select_ann(Some(id));
    }
}

/// 片頭、片尾卡片：開關、標題、副標、樣式、秒數，下面是預覽
fn cards_panel(ed: &mut Editor, ui: &mut egui::Ui, ctx: &egui::Context) {
    use screenrecorder_core::cards::{CardStyle, TitleCard};
    if ed.vw <= 0.0 {
        return hint(ui, tr!("無法讀取影片尺寸，不能加上片頭片尾。", "Couldn't read the video size, so title cards can't be added."));
    }
    let default_title = ed.default_card_title();
    for (i, (label, tip)) in [
        (
            tr!("加上片頭", "Add an intro"),
            tr!("影片開始前先顯示幾秒標題卡（淡入淡出），例如課程名稱、日期", "Show a title card for a few seconds before the video (with fades), e.g. the course name and date"),
        ),
        (tr!("加上片尾", "Add an outro"), tr!("影片結束後顯示幾秒卡片，例如「謝謝收看」、聯絡方式", "Show a card for a few seconds after the video, e.g. “Thanks for watching” or contact details")),
    ]
    .into_iter()
    .enumerate()
    {
        let slot = if i == 0 { &mut ed.spec.intro } else { &mut ed.spec.outro };
        let mut on = slot.is_some();
        if switch(ui, &mut on, label, true).on_hover_text(tip).changed() {
            *slot = on.then(|| TitleCard::new(if i == 0 { &default_title } else { tr!("謝謝收看", "Thanks for watching") }));
        }
        let Some(card) = slot.as_mut() else { continue };
        egui::Grid::new(("card-form", i)).num_columns(2).spacing(vec2(8.0, 6.0)).show(ui, |ui| {
            ui.label(theme::muted(ui, tr!("標題", "Title")).font(theme::font(12.0)));
            ui.add(egui::TextEdit::singleline(&mut card.title).desired_width(ui.available_width()).hint_text(tr!("例如：報價單的填法", "e.g. How to fill in a quote")));
            ui.end_row();
            ui.label(theme::muted(ui, tr!("副標", "Subtitle")).font(theme::font(12.0)));
            ui.add(egui::TextEdit::singleline(&mut card.subtitle).desired_width(ui.available_width()).hint_text(tr!("（可以不填）", "(optional)")));
            ui.end_row();
            ui.label(theme::muted(ui, tr!("樣式", "Style")).font(theme::font(12.0)));
            let items = [(CardStyle::Dark, tr!("深色", "Dark")), (CardStyle::Light, tr!("淺色", "Light")), (CardStyle::Blue, tr!("藍色", "Blue"))];
            segmented(ui, &mut card.style, &items, true);
            ui.end_row();
            ui.label(theme::muted(ui, tr!("長度", "Length")).font(theme::font(12.0)));
            theme::stepper(ui, Id::new(("card-secs", i)), &mut card.secs, 0.5, TitleCard::MIN_SECS..=TitleCard::MAX_SECS, 1, tr!(" 秒", " s"));
            ui.end_row();
        });
        // 預覽（和輸出一樣的畫法，縮小）
        let w = ui.available_width().min(260.0);
        let h = (w as f64 * ed.vh / ed.vw).round() as f32;
        let key = {
            use std::hash::{Hash, Hasher};
            let mut s = std::collections::hash_map::DefaultHasher::new();
            (&card.title, &card.subtitle, card.style as u8, w as u32, h as u32).hash(&mut s);
            s.finish()
        };
        if ed.card_tex[i].as_ref().is_none_or(|(k, _)| *k != key) {
            let ppp = ctx.pixels_per_point();
            let (pw, ph) = ((w * ppp) as u32, (h * ppp) as u32);
            ed.card_tex[i] = screenrecorder_core::cards::render(card, pw.max(2), ph.max(2)).map(|pm| {
                let img = egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data());
                (key, ctx.load_texture(format!("card-{i}"), img, egui::TextureOptions::LINEAR))
            });
        }
        if let Some((_, t)) = &ed.card_tex[i] {
            let (r, _) = ui.allocate_exact_size(vec2(w, h), Sense::hover());
            ui.painter().image(t.id(), r, egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            ui.painter().rect_stroke(r, CornerRadius::same(4), Stroke::new(1.0, theme::pal(ui).border), egui::StrokeKind::Outside);
        }
    }
}
