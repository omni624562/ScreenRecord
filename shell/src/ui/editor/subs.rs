//! 剪輯視窗的字幕（「字幕」分頁）：自動產生（語音辨識）或匯入 SRT，變成影片下方一段一段的文字標註；
//! 在清單裡直接改字，可以燒進影片，也可以依剪輯後的時間匯出成 SRT。

use super::shot::Act;
use super::{Editor, VAct};
use crate::ui::dialogs::{file_name, Ask};
use crate::ui::theme::{self, segmented, Btn};
use crate::ui::{settings_dialog, UiApp};
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, CornerRadius, CursorIcon, Id, Layout, RichText, Sense, Stroke};
use screenrecorder_core::annotate::{self, Ann, AnnKind};
use screenrecorder_core::components;
use screenrecorder_core::edit::normalize_ranges;
use screenrecorder_core::format::video_clock;
use screenrecorder_core::subtitles::{self, Cue, LANGUAGES, MODELS};
use screenrecorder_core::{tr, trf};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::Arc;

/// 一次最多加幾段（太多時輸出很慢）
const MAX_CUES: usize = 400;

/// 背景工作的進度
pub struct SubsJob {
    /// 0 檢查、1 下載語音模型、2 辨識語音、3 下載語音辨識用的 FFmpeg
    pub phase: Arc<AtomicU8>,
    /// 千分比
    pub progress: Arc<AtomicU32>,
    pub cancel: Arc<AtomicBool>,
}

/// 關掉剪輯視窗（或換一個工作）時停止：下載與辨識語音不會在背景一直跑
impl Drop for SubsJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// 剪輯視窗裡進行中的還是不是這個工作（關掉又開了別支影片時，結果不能加到別支影片上）
fn is_current(ed: &Editor, cancel: &Arc<AtomicBool>) -> bool {
    ed.subs.as_ref().is_some_and(|j| Arc::ptr_eq(&j.cancel, cancel))
}

/// 「字幕」分頁：產生或匯入字幕、選取的字幕的樣式與時間、逐段修改的清單
/// 字幕樣式：改了就套用到全部字幕
fn style_panel(ed: &mut Editor, ui: &mut egui::Ui) {
    let p = theme::pal(ui);
    let mut st = ed.sub_style.clone();
    egui::CollapsingHeader::new(RichText::new(tr!("字幕樣式", "Subtitle style")).font(theme::font_bold(13.0))).id_salt("ed-sub-style").show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(theme::muted(ui, tr!("大小", "Size")).font(theme::font(12.0)));
            segmented(ui, &mut st.size, &[(0u8, tr!("小", "Small")), (1, tr!("中", "Medium")), (2, tr!("大", "Large"))], true);
        });
        ui.horizontal(|ui| {
            ui.label(theme::muted(ui, tr!("位置", "Position")).font(theme::font(12.0)));
            segmented(ui, &mut st.top, &[(false, tr!("下方", "Bottom")), (true, tr!("上方", "Top"))], true);
        });
        ui.horizontal(|ui| {
            ui.label(theme::muted(ui, tr!("顏色", "Color")).font(theme::font(12.0)));
            for (c, name) in subtitles::SUB_COLORS.iter().zip([tr!("白色", "White"), tr!("黃色", "Yellow"), tr!("淺藍", "Light blue")]) {
                let (r, g, b) = annotate::parse_color(c);
                let col = Color32::from_rgb(r, g, b);
                let (rect, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
                ui.painter().circle_filled(rect.center(), 9.0, col);
                let on = st.color.eq_ignore_ascii_case(c);
                ui.painter().circle_stroke(rect.center(), 10.5, Stroke::new(if on { 2.0 } else { 1.0 }, if on { p.accent } else { p.border }));
                if resp.on_hover_text(name).on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    st.color = c.to_string();
                }
            }
            ui.add_space(8.0);
            theme::check(ui, &mut st.bg, RichText::new(tr!("深色底", "Dark background")).font(theme::font(12.5)));
        });
        hint(ui, tr!("改了會套用到全部字幕（個別移動過的也會放回原位）；之後加的字幕也用這個樣式。", "Changes apply to all subtitles (moved ones go back into place) and to subtitles you add later."));
    });
    if st != ed.sub_style {
        ed.sub_style = st;
        ed.apply_sub_style();
    }
}

pub fn tab(ed: &mut Editor, ui: &mut egui::Ui, ctx: &egui::Context, toast: &mut Option<(String, bool)>) {
    if ed.vw <= 0.0 {
        return hint(ui, tr!("無法讀取影片尺寸，不能加上字幕。", "Couldn't read the video size, so subtitles can't be added."));
    }
    panel(ed, ui);
    let n = ed.sub_ids.len();
    if n == 0 {
        return;
    }
    style_panel(ed, ui);
    ui.horizontal_wrapped(|ui| {
        if Btn::new(tr!("匯出字幕檔（依剪輯後的時間）", "Export subtitles (edited timing)"))
            .small()
            .tooltip(tr!(
                "把字幕存成 SRT 或 VTT 字幕檔（另存新檔時選類型），時間換成剪輯後的影片的時間（刪掉的片段裡的字幕不算）。SRT 給 YouTube，VTT 給網頁播放器、Teams、Stream",
                "Save the subtitles as an SRT or VTT file (pick the type when saving), with timing matched to the edited video (subtitles in removed sections are left out). SRT for YouTube, VTT for web players, Teams and Stream"
            ))
            .show(ui)
            .clicked()
        {
            ed.vpending = Some(VAct::ExportSrt);
        }
        let tr_btn = Btn::new(tr!("匯出逐字稿", "Export transcript"))
            .small()
            .tooltip(tr!(
                "把字幕整理成一段一段的文字（停比較久就換一段），存成 TXT 或 Word，適合會議記錄、上課筆記",
                "Turn the subtitles into paragraphs (a new one after each longer pause) and save as TXT or Word, for meeting minutes or class notes"
            ))
            .show(ui);
        egui::Popup::menu(&tr_btn).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
            ui.set_min_width(220.0);
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            theme::check(ui, &mut ed.transcript_time, RichText::new(tr!("每段加上時間", "Time at each paragraph")).font(theme::font(12.5)));
            theme::check(ui, &mut ed.transcript_clean, RichText::new(tr!("去掉「嗯、呃」等贅字", "Remove fillers like “um”")).font(theme::font(12.5)));
            ui.horizontal(|ui| {
                if Btn::new(tr!("存成 Word", "Save as Word")).primary().small().show(ui).clicked() {
                    ed.vpending = Some(VAct::ExportTranscript(true));
                    ui.close();
                }
                if Btn::new(tr!("存成 TXT", "Save as TXT")).small().show(ui).clicked() {
                    ed.vpending = Some(VAct::ExportTranscript(false));
                    ui.close();
                }
            });
        });
        if Btn::new(tr!("找出贅字和停頓", "Find fillers and pauses"))
            .small()
            .tooltip(tr!(
                "找出只有「嗯、呃、那個」的句子，和句子之間超過 1 秒沒講話的地方，勾選後一次剪掉（停頓前後各留一點，聽起來比較自然）",
                "Find sentences that are only “um”, “uh” and the like, and gaps of over 1 second between sentences, then cut the ones you tick (a little is kept around each pause so it sounds natural)"
            ))
            .show(ui)
            .clicked()
        {
            let found = ed.find_fillers();
            if found.is_empty() {
                *toast = Some((tr!("沒有找到贅字或停頓", "No fillers or pauses found").into(), false));
                ed.fillers = None;
            } else {
                ed.fillers = Some(found.into_iter().map(|f| {
                    let on = f.default_on();
                    (f, on)
                }).collect());
            }
        }
        if Btn::new(tr!("依字幕產生章節", "Chapters from subtitles"))
            .small()
            .tooltip(tr!(
                "在講話停頓最久的地方切成幾章（每章至少 30 秒），用那一章第一句當標題，變成時間軸上的標記；輸出時寫進影片，也能貼到 YouTube 說明欄",
                "Split the video into chapters at the longest pauses in speech (at least 30 s each), titled with each chapter's first line. They become markers on the timeline and are written into the output video"
            ))
            .show(ui)
            .clicked()
        {
            *toast = Some((ed.chapters_from_subs(), false));
        }
        if Btn::new(tr!("全部刪除", "Delete all")).ghost().small().tooltip(tr!("刪掉所有字幕（可以按 Ctrl+Z 復原）", "Delete all subtitles (press Ctrl+Z to undo)")).show(ui).clicked()
        {
            let ids = std::mem::take(&mut ed.sub_ids);
            ed.anns.retain(|a| !ids.contains(&a.id));
            if ed.ann_sel.is_some_and(|id| ids.contains(&id)) {
                ed.ann_sel = None;
            }
        }
    });
    fillers_panel(ed, ui, toast);
    // 選取的字幕：顏色、字級、深色底（可以套用到全部字幕）、出現時間
    let picked: Vec<u64> = ed.sub_picked.iter().copied().filter(|id| ed.is_sub(*id)).collect();
    if ed.ann_sel.is_some_and(|id| ed.is_sub(id)) {
        super::side::props(ed, ui, ctx);
    } else {
        // 說明和「選了好幾句」的按鈕放在同一個位置、一樣高，選句子時下面的清單不會跳動
        let hint_h = Id::new("sub-hint-h");
        let h = ui.data(|d| d.get_temp::<f32>(hint_h)).unwrap_or(0.0);
        if picked.is_empty() {
            let r = ui
                .scope(|ui| {
                    hint(
                        ui,
                        tr!(
                            "用文字剪影片：按句子右邊的剪刀就剪掉那一句講話的片段；Ctrl 或 Shift＋點時間可以選好幾句一起剪。點時間跳過去，直接在文字上修改。",
                            "Edit by text: click the scissors next to a sentence to cut that part of the video; Ctrl- or Shift-click times to pick several. Click a time to jump there; edit the text right in the list."
                        ),
                    )
                })
                .response
                .rect;
            ui.data_mut(|d| d.insert_temp(hint_h, r.height()));
        } else {
            let w = ui.available_width();
            ui.allocate_ui_with_layout(vec2(w, h), Layout::left_to_right(Align::Center), |ui| {
                ui.set_min_height(h);
                ui.label(RichText::new(trf!("已選 {} 句", "{} selected", picked.len())).font(theme::font(12.5)));
                if Btn::new(trf!("剪掉這 {} 句", "Cut {} sentences", picked.len())).icon(theme::Icon::Cut).small().primary().show(ui).clicked() {
                    *toast = Some((ed.cut_subs(&picked), false));
                }
                if Btn::new(tr!("取消選取", "Clear selection")).ghost().small().show(ui).clicked() {
                    ed.sub_picked.clear();
                }
            });
        }
    }
    list(ed, ui, toast);
}

/// 找到的贅字與停頓：勾選要剪的，一次剪掉
fn fillers_panel(ed: &mut Editor, ui: &mut egui::Ui, toast: &mut Option<(String, bool)>) {
    use screenrecorder_core::fillers::{Kind, LONG_PAUSE};
    let Some(list) = ed.fillers.as_mut() else { return };
    let p = theme::pal(ui);
    let (mut close, mut cut, mut seek) = (false, false, None);
    egui::Frame::new().fill(p.surface2).corner_radius(CornerRadius::same(8)).inner_margin(10.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(trf!("贅字和停頓（{} 段）", "Fillers and pauses ({})", list.len())).font(theme::font_bold(13.0)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if Btn::icon_only(theme::Icon::Close).ghost().small().tooltip(tr!("關閉", "Close")).show(ui).clicked() {
                    close = true;
                }
                let all = list.iter().all(|x| x.1);
                if Btn::new(if all { tr!("全部不選", "Select none") } else { tr!("全選", "Select all") }).ghost().small().show(ui).clicked() {
                    for x in list.iter_mut() {
                        x.1 = !all;
                    }
                }
            });
        });
        egui::ScrollArea::vertical().id_salt("fillers").max_height(200.0).show(ui, |ui| {
            for (f, on) in list.iter_mut() {
                ui.horizontal(|ui| {
                    theme::check(ui, on, "");
                    let t = ui
                        .add(egui::Label::new(RichText::new(video_clock(f.start)).font(theme::mono(11.5)).color(p.accent)).sense(Sense::click()))
                        .on_hover_text(tr!("跳到這裡聽聽看", "Jump here to listen"))
                        .on_hover_cursor(CursorIcon::PointingHand);
                    if t.clicked() {
                        seek = Some(f.start);
                    }
                    let text = match f.kind {
                        Kind::Word => trf!("「{}」", "“{}”", f.text),
                        Kind::Pause if f.len() > LONG_PAUSE => trf!("停頓 {:.1} 秒（可能在操作畫面）", "{:.1} s pause (you may be showing something)", f.len()),
                        Kind::Pause => trf!("停頓 {:.1} 秒", "{:.1} s pause", f.len()),
                    };
                    ui.label(RichText::new(text).font(theme::font(12.5)));
                });
            }
        });
        let picked: Vec<(f64, f64)> = list.iter().filter(|x| x.1).map(|x| (x.0.start, x.0.end)).collect();
        let secs: f64 = picked.iter().map(|(a, b)| b - a).sum();
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let b = Btn::new(trf!("剪掉勾選的 {} 段（共 {}）", "Cut {} selected ({})", picked.len(), video_clock(secs))).icon(theme::Icon::Cut).primary().small().enabled(!picked.is_empty());
            if b.show(ui).clicked() {
                cut = true;
            }
        });
    });
    ui.add_space(6.0);
    if let Some(t) = seek {
        ed.seek(t);
    }
    if cut {
        *toast = Some((ed.cut_fillers(), false));
    } else if close {
        ed.fillers = None;
    }
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(theme::muted(ui, text).font(theme::font(12.0)));
}

/// 每一段字幕一行：時間（點一下跳過去並選取；Ctrl / Shift＋點選好幾句）、可以直接修改的文字、
/// 剪掉這一句（剪掉的顯示還原）、刪除字幕
fn list(ed: &mut Editor, ui: &mut egui::Ui, toast: &mut Option<(String, bool)>) {
    let p = theme::pal(ui);
    let keep = ed.keep();
    let mut rows: Vec<(u64, f64, f64, String, bool)> =
        ed.anns.iter().filter(|a| ed.is_sub(a.id)).map(|a| (a.id, a.start, a.end, subtitles::flat(&a.text.clone().unwrap_or_default()), !Editor::in_output(a, &keep))).collect();
    rows.sort_by(|x, y| x.1.total_cmp(&y.1).then(x.0.cmp(&y.0)));
    let now = ed.now();
    let order: Vec<u64> = rows.iter().map(|r| r.0).collect();
    let mods = ui.input(|i| i.modifiers);
    let (mut select, mut delete, mut edit, mut seek, mut cut, mut uncut, mut pick) = (None, None, None, None, None, None, None);
    let w = ui.available_width();
    ui.spacing_mut().item_spacing.y = 3.0;
    for (id, start, end, mut text, gone) in rows {
        let (r, _) = ui.allocate_exact_size(vec2(w, 30.0), Sense::hover());
        // 看不到的那幾行不畫（字幕可能有幾百段）
        if !ui.is_rect_visible(r) {
            continue;
        }
        let on = ed.ann_sel == Some(id);
        let picked = ed.sub_picked.contains(&id);
        let playing_here = now >= start && now < end;
        let painter = ui.painter();
        let fill = if picked {
            p.accent_soft
        } else if on {
            p.accent.gamma_multiply(0.12)
        } else {
            Color32::TRANSPARENT
        };
        painter.rect(r, CornerRadius::same(6), fill, Stroke::new(1.0, if on || picked { p.accent } else { p.border }), egui::StrokeKind::Inside);
        if playing_here {
            painter.rect_filled(egui::Rect::from_min_size(r.min + vec2(1.0, 5.0), vec2(3.0, r.height() - 10.0)), CornerRadius::same(2), p.accent);
        }
        let tr_rect = egui::Rect::from_min_size(r.min, vec2(62.0, r.height()));
        let t = ui.interact(tr_rect, Id::new(("sub-time", id)), Sense::click());
        let tcolor = if gone {
            p.muted
        } else if t.hovered() {
            p.accent
        } else {
            p.text
        };
        ui.painter().text(pos2(tr_rect.left() + 8.0, r.center().y), Align2::LEFT_CENTER, video_clock(start), theme::mono(11.5), tcolor);
        let gone_note = if gone { tr!("（在刪除的片段中，不會出現）", " (in a removed section; won't appear)") } else { "" };
        if t.on_hover_cursor(CursorIcon::PointingHand)
            .on_hover_text(trf!(
                "{} – {}{}・點一下跳到這一段並選取；Ctrl / Shift＋點選好幾句一起剪",
                "{} – {}{} · click to jump here and select it; Ctrl/Shift-click to pick several to cut",
                video_clock(start),
                video_clock(end),
                gone_note
            ))
            .clicked()
        {
            if mods.command || mods.shift {
                pick = Some((id, mods.shift));
            } else {
                select = Some((id, start));
            }
        }
        let er = egui::Rect::from_min_max(pos2(r.left() + 64.0, r.top() + 3.0), pos2(r.right() - 56.0, r.bottom() - 3.0));
        let mut te = egui::TextEdit::singleline(&mut text).id(Id::new(("sub-text", id))).font(theme::font(13.0)).margin(vec2(4.0, 3.0));
        if gone {
            te = te.text_color(p.muted);
        }
        let resp = ui.put(er, te);
        // 剪掉這一句（剪掉的顯示還原）
        let c_rect = egui::Rect::from_center_size(pos2(r.right() - 41.0, r.center().y), vec2(22.0, 22.0));
        let tip = if gone {
            tr!("還原這一句（把這句講話的片段放回影片）", "Restore this sentence (put this part back into the video)")
        } else {
            tr!("剪掉這一句講話的片段", "Cut this part of the video")
        };
        let cr = ui.interact(c_rect, Id::new(("sub-cut", id)), Sense::click()).on_hover_text(tip).on_hover_cursor(CursorIcon::PointingHand);
        if cr.hovered() {
            ui.painter().circle_filled(c_rect.center(), 11.0, p.accent_soft);
        }
        theme::paint_icon(
            ui.painter(),
            egui::Rect::from_center_size(c_rect.center(), vec2(14.0, 14.0)),
            if gone { theme::Icon::Undo } else { theme::Icon::Cut },
            if cr.hovered() { p.accent } else { p.muted },
        );
        if cr.clicked() {
            if gone {
                uncut = Some(id);
            } else {
                cut = Some(id);
            }
        }
        if resp.changed() {
            edit = Some((id, text));
        }
        // 開始改字：影片跳到這一段（不選取，上面不會多出設定、清單不會跳動）
        if resp.gained_focus() && (now < start || now > end) {
            seek = Some(start);
        }
        let x_rect = egui::Rect::from_center_size(pos2(r.right() - 15.0, r.center().y), vec2(22.0, 22.0));
        let xr = ui.interact(x_rect, Id::new(("sub-del", id)), Sense::click()).on_hover_text(tr!("刪除這段字幕", "Delete this subtitle")).on_hover_cursor(CursorIcon::PointingHand);
        if xr.hovered() {
            ui.painter().circle_filled(x_rect.center(), 11.0, p.rec_soft);
        }
        ui.painter().text(x_rect.center(), Align2::CENTER_CENTER, "×", theme::font(14.0), if xr.hovered() { p.rec } else { p.muted });
        if xr.clicked() {
            delete = Some(id);
        }
    }
    if let Some((id, text)) = edit {
        ed.set_sub_text(id, &text);
    }
    if let Some(id) = cut {
        *toast = Some((ed.cut_subs(&[id]), false));
    }
    if let Some(id) = uncut {
        ed.uncut_sub(id);
        *toast = Some((tr!("已還原這一句，可按 Ctrl+Z 復原", "Sentence restored. Press Ctrl+Z to undo").into(), false));
    }
    // Ctrl＋點：加入或拿掉；Shift＋點：從上次點的那句選到這句
    if let Some((id, range)) = pick {
        let anchor = ed.sub_anchor.and_then(|a| order.iter().position(|x| *x == a));
        match (range, anchor, order.iter().position(|x| *x == id)) {
            (true, Some(a), Some(b)) => {
                for x in &order[a.min(b)..=a.max(b)] {
                    ed.sub_picked.insert(*x);
                }
            }
            _ => {
                if !ed.sub_picked.remove(&id) {
                    ed.sub_picked.insert(id);
                }
            }
        }
        ed.sub_anchor = Some(id);
        // 收起單一字幕的設定，上面才看得到「剪掉這幾句」
        ed.ann_sel = None;
    }
    if let Some(t) = seek {
        ed.seek(t);
    }
    if let Some(id) = delete {
        ed.delete_ann(id);
    }
    if let Some((id, start)) = select {
        if ed.ann_sel != Some(id) {
            let now = ed.now();
            if ed.anns.iter().find(|a| a.id == id).is_none_or(|a| now < a.start || now > a.end) {
                ed.seek(start);
            }
            ed.select_ann(Some(id));
        }
    }
}

/// 產生或匯入字幕的按鈕（「字幕」分頁最上面）
pub fn panel(ed: &mut Editor, ui: &mut egui::Ui) {
    let p = theme::pal(ui);
    ui.add_space(2.0);
    ui.label(RichText::new(tr!("字幕", "Subtitles")).font(theme::font_bold(13.5)));
    if let Some(job) = &ed.subs {
        let pct = job.progress.load(Ordering::Relaxed) as f32 / 10.0;
        let text = match job.phase.load(Ordering::Relaxed) {
            0 => tr!("正在確認 FFmpeg 支不支援語音辨識…", "Checking whether FFmpeg supports speech recognition…").to_string(),
            4 => trf!("正在翻成英文… {pct:.0}%", "Translating to English… {pct:.0}%"),
            _ => trf!("正在辨識語音… {pct:.0}%", "Recognizing speech… {pct:.0}%"),
        };
        ui.horizontal(|ui| {
            ui.add(egui::ProgressBar::new(pct / 100.0).desired_width(ui.available_width() - 70.0).text(RichText::new(text).font(theme::font(12.0))));
            if Btn::new(tr!("取消", "Cancel")).ghost().small().show(ui).clicked() {
                ed.pending = Some(Act::CancelSubs);
            }
        });
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        return;
    }
    ui.horizontal(|ui| {
        let auto = Btn::new(tr!("自動產生字幕", "Generate subtitles"))
            .small()
            .tooltip(tr!(
                "在這台電腦上辨識影片裡說的話（不用上傳），產生一段一段的字幕，可以修改後燒進影片；也會在影片旁邊存一份 SRT",
                "Recognize the speech in the video on this computer (nothing is uploaded) and turn it into subtitles you can edit and burn into the video; an SRT copy is also saved next to the video"
            ))
            .show(ui);
        egui::Popup::menu(&auto).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
            ui.set_min_width(260.0);
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            ui.label(theme::muted(ui, tr!("說的語言", "Spoken language")).font(theme::font(12.0)));
            let items: Vec<(usize, &str)> = LANGUAGES.iter().enumerate().map(|(i, _)| (i, subtitles::language_name(i))).collect();
            segmented(ui, &mut ed.subs_lang, &items, true);
            ui.label(theme::muted(ui, tr!("辨識的品質", "Recognition quality")).font(theme::font(12.0)));
            let items: Vec<(usize, &str)> = MODELS.iter().enumerate().map(|(i, m)| (i, m.short())).collect();
            segmented(ui, &mut ed.subs_model, &items, true);
            ui.label(theme::muted(ui, tr!("字幕", "Subtitles")).font(theme::font(12.0)));
            let items = [(0, tr!("原文", "Original")), (1, tr!("翻成英文", "English")), (2, tr!("中英對照", "Both"))];
            segmented(ui, &mut ed.subs_out, &items, true);
            if ed.subs_out == 2 {
                ui.label(theme::muted(ui, tr!("辨識兩次（原文、英文），時間大約多一倍", "Recognizes twice (original and English), so it takes about twice as long")).font(theme::font(12.0)));
            }
            let translating = ed.subs_out != 0 && LANGUAGES[ed.subs_lang.min(LANGUAGES.len() - 1)].0 != "en";
            let mut need = components::missing_for_subtitles(ed.ffmpeg_whisper, ed.subs_model);
            if translating {
                if !MODELS[ed.subs_model.min(MODELS.len() - 1)].translates {
                    let note =
                        tr!("「最精準」不能翻成英文，英文的部分會改用「精準」或「標準」模型", "\"Most accurate\" can't translate, so the English part uses the \"Accurate\" or \"Standard\" model");
                    ui.label(theme::muted(ui, note).font(theme::font(12.0)));
                }
                need.extend(components::missing_for_translation(ed.subs_model));
            }
            if !need.is_empty() {
                let note = trf!("要先到「設定 → 外部元件」下載{}", "First download {} in Settings → Components", components::describe(&need));
                ui.label(RichText::new(note).font(theme::font(12.0)).color(theme::pal(ui).warn));
            }
            if Btn::new(tr!("開始", "Start")).primary().small().show(ui).clicked() {
                ed.pending = Some(Act::AutoSubs);
                ui.close();
            }
        });
        if Btn::new(tr!("匯入字幕檔…", "Import subtitle file…"))
            .small()
            .tooltip(tr!(
                "匯入 SRT 或 VTT 字幕檔（例如 YouTube、Teams 或其他工具產生的），變成可以修改的字幕",
                "Import an SRT or VTT subtitle file (e.g. from YouTube, Teams or another tool) as subtitles you can edit"
            ))
            .show(ui)
            .clicked()
        {
            ed.pending = Some(Act::ImportSrt);
        }
    });
    if ed.sub_ids.is_empty() {
        ui.label(
            RichText::new(tr!(
                "字幕會放在影片下方，之後在這裡的清單逐段修改；不滿意可以按 Ctrl+Z 復原。",
                "Subtitles are placed at the bottom of the video; edit them one by one in the list here. Press Ctrl+Z to undo."
            ))
            .font(theme::font(12.0))
            .color(p.muted),
        );
    }
}

pub fn run(app: &mut UiApp, ed: &mut Editor, act: Act) {
    match act {
        Act::CancelSubs => {
            if let Some(j) = &ed.subs {
                j.cancel.store(true, Ordering::Relaxed);
            }
        }
        Act::ImportSrt => {
            let video = ed.entry.media.path.clone();
            app.spawn(
                async move {
                    let path = tokio::task::spawn_blocking(subtitles::pick_file).await.unwrap_or(Ok(None))?;
                    let Some(path) = path else { return Ok(None) };
                    subtitles::read_file(&path).map(|c| Some((c, file_name(&path.to_string_lossy()))))
                },
                move |app, r: Result<Option<(Vec<Cue>, String)>, String>| match r {
                    Ok(Some((cues, name))) => {
                        // 選檔案時關掉或換了影片：不加
                        let Some(ed) = app.editor.as_mut().filter(|e| e.entry.media.path == video) else { return };
                        let n = ed.add_subtitles(&cues);
                        let more = if cues.len() > n { trf!("（太多了，只加前 {MAX_CUES} 段）", " (too many; only the first {MAX_CUES} were added)") } else { String::new() };
                        let msg = if screenrecorder_core::i18n::is_en() {
                            format!("Added {n} subtitle{} from “{name}”{more}", if n == 1 { "" } else { "s" })
                        } else {
                            format!("已從「{name}」加上 {n} 段字幕{more}")
                        };
                        app.toast(msg, false);
                    }
                    Ok(None) => {}
                    Err(e) => app.toast(e, true),
                },
            );
        }
        Act::AutoSubs => {
            if ed.subs.is_some() {
                return;
            }
            let job = SubsJob { phase: Arc::new(AtomicU8::new(0)), progress: Arc::new(AtomicU32::new(0)), cancel: Arc::new(AtomicBool::new(false)) };
            let mine = job.cancel.clone();
            let ffmpeg = ed.ffmpeg.clone();
            ed.subs = Some(job);
            // 語音辨識用的 FFmpeg、語音模型都在「設定 → 外部元件」下載；還沒下載時請使用者先去下載
            app.spawn(async move { subtitles::find_speech_ffmpeg(&ffmpeg).await }, move |app, speech| {
                let Some(ed) = app.editor.as_mut().filter(|e| is_current(e, &mine)) else { return };
                let mut need = components::missing_for_subtitles(true, ed.subs_model);
                if ed.subs_out != 0 && LANGUAGES[ed.subs_lang.min(LANGUAGES.len() - 1)].0 != "en" {
                    need.extend(components::missing_for_translation(ed.subs_model));
                }
                if speech.is_none() {
                    need.insert(0, components::Kind::SpeechFfmpeg);
                }
                match speech {
                    Some(speech) if need.is_empty() => start(app, speech),
                    _ => {
                        ed.subs = None;
                        let msg = trf!(
                            "自動產生字幕要先下載{}。\n\n請到「設定 → 外部元件」下載（下載不了時，也可以在那裡選擇自己下載的檔案）。辨識在這台電腦上進行，影片不會上傳。",
                            "Auto subtitles need {} first.\n\nDownload it in Settings → Components (if the download doesn't work, you can choose a file you downloaded yourself there). Recognition runs on this computer; the video isn't uploaded.",
                            components::describe(&need)
                        );
                        app.ask = Some(Ask::confirm(tr!("需要先下載語音辨識的檔案", "Speech recognition files needed"), msg, tr!("前往外部元件", "Go to Components"), |app, _| {
                            app.ask = None;
                            settings_dialog::open(app, settings_dialog::Page::Components);
                        }));
                    }
                }
            });
        }
        _ => {}
    }
}

/// 辨識語音（需要的檔案都已下載）；完成後加上字幕、在影片旁邊存一份 SRT
fn start(app: &mut UiApp, speech: PathBuf) {
    let Some(ed) = &mut app.editor else { return };
    let Some(job) = &ed.subs else { return };
    let (phase, progress, cancel) = (job.phase.clone(), job.progress.clone(), job.cancel.clone());
    let mine = job.cancel.clone();
    let m = MODELS[ed.subs_model.min(MODELS.len() - 1)];
    let lang = LANGUAGES[ed.subs_lang.min(LANGUAGES.len() - 1)].0;
    // 說英文時「翻成英文」就是原文
    let out = if lang == "en" { 0 } else { ed.subs_out };
    let (video, duration) = (ed.entry.media.path.clone(), ed.duration);
    app.spawn(
        async move {
            let orig = if out != 1 {
                phase.store(2, Ordering::Relaxed);
                progress.store(0, Ordering::Relaxed);
                subtitles::transcribe(&speech, &video, duration, &m, lang, progress.clone(), cancel.clone()).await?
            } else {
                vec![]
            };
            let en = if out != 0 && !(out == 2 && orig.is_empty()) {
                phase.store(4, Ordering::Relaxed);
                progress.store(0, Ordering::Relaxed);
                subtitles::translate(&speech, &video, duration, &m, lang, progress, cancel).await?
            } else {
                vec![]
            };
            // 原文存成「同名.srt」、英文存成「同名.en.srt」
            let mut saved = vec![];
            if !orig.is_empty() {
                saved.extend(subtitles::save_next_to(&video, &orig).ok());
            }
            if !en.is_empty() {
                saved.extend(subtitles::save_english_next_to(&video, &en).ok());
            }
            let cues = match out {
                1 => en,
                2 => subtitles::pair(&orig, &en),
                _ => orig,
            };
            Ok::<_, String>((cues, saved))
        },
        move |app, r| {
            let Some(ed) = app.editor.as_mut().filter(|e| is_current(e, &mine)) else { return };
            // 按了取消（在 ed.subs 清掉之前讀：SubsJob 結束時也會設成取消）；core 的錯誤訊息可能是英文，不只比對文字
            let canceled = mine.load(Ordering::Relaxed);
            ed.subs = None;
            match r {
                Ok((cues, _)) if cues.is_empty() => app.toast(tr!("沒有辨識出說話的內容", "No speech was recognized"), false),
                Ok((cues, srt)) => {
                    let n = ed.add_subtitles(&cues);
                    let names = srt.iter().map(|p| file_name(&p.to_string_lossy())).collect::<Vec<_>>().join(tr!("」「", "”, “"));
                    let saved = if names.is_empty() { String::new() } else { trf!("，也存了一份「{}」", " and saved a copy as “{}”", names) };
                    let msg = if screenrecorder_core::i18n::is_en() {
                        format!("Added {n} subtitle{}{saved}. Check for typos; click a subtitle to edit it", if n == 1 { "" } else { "s" })
                    } else {
                        format!("加上 {n} 段字幕{saved}；請檢查錯字，點一下字幕就能修改")
                    };
                    app.toast(msg, false);
                }
                Err(e) if canceled || e.contains("取消") => app.toast(tr!("已取消產生字幕", "Subtitle generation canceled"), false),
                Err(e) => app.toast(e, true),
            }
        },
    );
}

impl Editor {
    /// 用文字剪：剪掉這幾句字幕的時間（加到刪除的片段，前後多留一點點不切到字）；回傳要顯示的訊息
    pub(crate) fn cut_subs(&mut self, ids: &[u64]) -> String {
        let spans: Vec<(f64, f64)> = self.anns.iter().filter(|a| ids.contains(&a.id) && self.is_sub(a.id)).map(|a| (a.start, a.end)).collect();
        if spans.is_empty() {
            return String::new();
        }
        let mut list = self.spec.removed.clone();
        list.extend(spans.iter().copied());
        self.spec.removed = normalize_ranges(&list, self.duration);
        self.sub_picked.clear();
        self.sel = None;
        let secs: f64 = spans.iter().map(|(a, b)| b - a).sum();
        let n = spans.len();
        if screenrecorder_core::i18n::is_en() {
            format!("Cut {n} sentence{} ({}). Press Ctrl+Z to undo", if n == 1 { "" } else { "s" }, video_clock(secs))
        } else {
            format!("剪掉 {n} 句（共 {}），可按 Ctrl+Z 復原", video_clock(secs))
        }
    }

    /// 從字幕找出贅字與停頓（已經剪掉的不算）
    pub(crate) fn find_fillers(&self) -> Vec<screenrecorder_core::fillers::Filler> {
        let cues: Vec<Cue> = self.anns.iter().filter(|a| self.is_sub(a.id)).map(|a| Cue { start: a.start, end: a.end, text: a.text.clone().unwrap_or_default() }).collect();
        screenrecorder_core::fillers::find(&cues, &self.spec.removed, self.duration)
    }

    /// 剪掉勾選的贅字與停頓；回傳要顯示的訊息
    pub(crate) fn cut_fillers(&mut self) -> String {
        let Some(list) = self.fillers.take() else { return String::new() };
        let spans: Vec<(f64, f64)> = list.iter().filter(|x| x.1).map(|x| (x.0.start, x.0.end)).collect();
        let mut all = self.spec.removed.clone();
        all.extend(spans.iter().copied());
        self.spec.removed = normalize_ranges(&all, self.duration);
        self.sel = None;
        let secs: f64 = spans.iter().map(|(a, b)| b - a).sum();
        trf!("剪掉 {} 段贅字和停頓（共 {}），可按 Ctrl+Z 復原", "Cut {} fillers and pauses ({}). Press Ctrl+Z to undo", spans.len(), video_clock(secs))
    }

    /// 還原剪掉的這一句（只還原這句的時間，前後剪掉的部分不動）
    pub(crate) fn uncut_sub(&mut self, id: u64) {
        let Some((a, b)) = self.anns.iter().find(|x| x.id == id).map(|x| (x.start, x.end)) else { return };
        self.spec.removed = screenrecorder_core::edit::unremove(&self.spec.removed, (a, b), self.duration);
        // 剪掉頭尾的部分裡：把頭尾放寬到包含這一句
        if a < self.spec.start {
            self.spec.start = a;
        }
        if b > self.spec.end {
            self.spec.end = b;
        }
    }

    /// 改一段字幕的文字（太長自動斷成兩行）：寬度變了以下緣中間為準，位置不跑掉
    pub fn set_sub_text(&mut self, id: u64, text: &str) {
        let Some(a) = self.ann_mut(id) else { return };
        let (cx, bottom) = (a.x + a.w / 2.0, a.y + a.h);
        a.text = Some(subtitles::unflat(text));
        annotate::measure(a);
        a.x = cx - a.w / 2.0;
        a.y = bottom - a.h;
    }

    /// 依字幕產生章節標記（取代離新章節很近的舊標記）；回傳要顯示的訊息
    pub fn chapters_from_subs(&mut self) -> String {
        let mut cues: Vec<Cue> = self.anns.iter().filter(|a| self.is_sub(a.id)).map(|a| Cue { start: a.start, end: a.end, text: a.text.clone().unwrap_or_default() }).collect();
        cues.sort_by(|x, y| x.start.total_cmp(&y.start));
        let marks = screenrecorder_core::chapters::from_subtitles(&cues, self.duration);
        if marks.is_empty() {
            return tr!("影片太短或講話沒有明顯的停頓，沒辦法分章節", "The video is too short or has no clear pauses in speech, so it can't be split into chapters").into();
        }
        self.markers.retain(|m| marks.iter().all(|x| (x.t - m.t).abs() >= 2.0));
        let n = marks.len();
        self.markers.extend(marks);
        self.markers.sort_by(|a, b| a.t.total_cmp(&b.t));
        trf!(
            "分成 {n} 章，標在時間軸上（按兩下旗子可以改名），輸出時寫進影片",
            "Split into {n} chapters, marked on the timeline (double-click a flag to rename); they're written into the output video"
        )
    }

    /// 字幕換成剪輯後的時間（刪掉的片段裡的不算；跨過刪掉的片段時從第一段保留的開始到最後一段保留的結束）
    pub fn cut_cues(&self) -> Vec<Cue> {
        let parts = self.parts();
        // 有片頭時字幕往後移
        let shift = self.spec.intro.as_ref().filter(|c| !c.is_empty()).map_or(0.0, |c| c.secs());
        let mut subs: Vec<&Ann> = self.anns.iter().filter(|a| self.is_sub(a.id)).collect();
        subs.sort_by(|x, y| x.start.total_cmp(&y.start));
        subs.into_iter()
            .filter_map(|a| {
                let over: Vec<&screenrecorder_core::edit::Part> = parts.iter().filter(|p| !p.is_hold() && p.a < a.end && p.b > a.start).collect();
                let (first, last) = (over.first()?, over.last()?);
                let start = screenrecorder_core::zoom::map_time(&parts, a.start.max(first.a))?;
                let end = screenrecorder_core::zoom::map_time(&parts, (a.end.min(last.b) - 0.001).max(first.a))? + 0.001;
                (end - start >= 0.05).then(|| Cue { start: start + shift, end: end + shift, text: a.text.clone().unwrap_or_default() })
            })
            .collect()
    }

    /// 字幕樣式套用到全部字幕（字級、顏色、底色、位置；手動移過的也放回去）
    pub fn apply_sub_style(&mut self) {
        let style = self.sub_style.clone();
        let (vw, vh) = (self.vw, self.vh);
        let ids = self.sub_ids.clone();
        for a in self.anns.iter_mut().filter(|a| ids.contains(&a.id)) {
            a.color = style.color.clone();
            a.bg = style.bg;
            a.size = style.font_px(vh);
            a.rot = 0.0;
            annotate::measure(a);
            (a.x, a.y) = style.place(a.w, a.h, vw, vh);
        }
    }

    /// 字幕變成文字標註：依字幕樣式（預設白字深色底，放在影片下方中間）；回傳加了幾段
    pub fn add_subtitles(&mut self, cues: &[Cue]) -> usize {
        if self.vw <= 0.0 {
            return 0;
        }
        let style = self.sub_style.clone();
        let mut n = 0;
        for c in cues.iter().take(MAX_CUES) {
            let (start, end) = (c.start.clamp(0.0, self.duration), c.end.clamp(0.0, self.duration));
            if end - start < 0.05 {
                continue;
            }
            // 中文一行約 22 字、英文約 42 個字母
            let text = subtitles::wrap_lines(&c.text);
            let mut a = Ann {
                id: self.next_id,
                kind: AnnKind::Text,
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
                start,
                end,
                color: style.color.clone(),
                size: style.font_px(self.vh),
                text: Some(text),
                bg: style.bg,
                n: None,
                shape: None,
                invert: false,
                rot: 0.0,
                pts: vec![],
                anim: 0,
                bend: 0.0,
            };
            annotate::measure(&mut a);
            (a.x, a.y) = style.place(a.w, a.h, self.vw, self.vh);
            self.next_id += 1;
            self.sub_ids.insert(a.id);
            self.anns.push(a);
            n += 1;
        }
        if n > 0 {
            self.ann_sel = None;
            self.tool = None;
            self.tab = super::Tab::Subs;
        }
        n
    }
}
