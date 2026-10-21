//! 剪輯視窗「加速」分頁：整支加速（倍率、自訂、做成指定長度）、選取的片段加速或放慢、定格、
//! 變速片段的清單（自動找出沒動靜的片段在「修剪」分頁的「清掉空白」）

use super::side::{hint, section, section_title};
use super::Editor;
use crate::ui::theme::{self, Btn};
use eframe::egui::{self, pos2, vec2, Align2, Color32, CornerRadius, CursorIcon, Id, RichText, Sense};
use screenrecorder_core::edit::{output_length, set_fast, valid_speed, Freeze, FAST_SPEEDS, FREEZE_SECS, SLOW_SPEEDS, WHOLE_SPEEDS};
use screenrecorder_core::format::{parse_clock, speed_label, video_clock};
use screenrecorder_core::{tr, trf};

/// 整支加速的上限
const WHOLE_MAX: f64 = 16.0;

pub(super) fn panel(ed: &mut Editor, ui: &mut egui::Ui, toast: &mut Option<(String, bool)>) {
    whole(ed, ui);
    sections(ed, ui);
    freezes(ed, ui);
    section(ui, tr!("自動加速", "Automatic"), false);
    hint(
        ui,
        tr!(
            "要把等待、打字的地方自動加速：到「修剪」分頁的「清掉空白」找出來，再選 2×、4×、8×。",
            "To speed up waiting and typing automatically, use “Clean up blank parts” in the Trim tab, then pick 2×, 4× or 8×."
        ),
    );
    if Btn::new(tr!("前往清掉空白", "Go to Clean up")).small().ghost().show(ui).clicked() {
        ed.tab = super::Tab::Time;
    }
    super::side::found_panel(ed, ui, toast);
}

/// 剪輯後的長度拆成「會被整支加速影響的」與定格（不受影響）
fn lengths(ed: &Editor) -> (f64, f64) {
    let parts = ed.parts();
    let holds: f64 = parts.iter().filter(|p| p.is_hold()).map(|p| p.hold).sum();
    let whole = screenrecorder_core::edit::whole_speed(&ed.spec);
    ((output_length(&parts) - holds) * whole, holds)
}

fn set_whole(ed: &mut Editor, s: f64) {
    let s = ((s.clamp(1.0, WHOLE_MAX)) * 100.0).round() / 100.0;
    let was = ed.spec.speed;
    ed.spec.speed = s;
    // 2 倍以內講話還聽得懂：預設保留聲音（從原速改過來時才改，自己勾過的不動）
    if (was - 1.0).abs() < 1e-9 || (was <= 2.0) != (s <= 2.0) {
        ed.spec.speed_audio = s <= 2.0;
    }
}

/// 整支加速：常用倍率、自訂倍率、做成指定長度
fn whole(ed: &mut Editor, ui: &mut egui::Ui) {
    section_title(ui, tr!("整支加速", "Speed up the whole video"));
    let cur = screenrecorder_core::edit::whole_speed(&ed.spec);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
        for s in WHOLE_SPEEDS {
            let label = if s == 1.0 { tr!("原速", "1×").to_string() } else { format!("{}×", speed_label(s)) };
            if Btn::new(&label).small().selected((s - cur).abs() < 1e-9).show(ui).clicked() {
                set_whole(ed, s);
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label(theme::muted(ui, tr!("自訂倍率", "Custom")).font(theme::font(12.0)));
        let mut v = cur;
        if ui.add(egui::DragValue::new(&mut v).range(1.0..=WHOLE_MAX).speed(0.05).fixed_decimals(2).suffix("×")).changed() {
            set_whole(ed, v);
        }
    });
    // 做成指定長度：依剪好的長度算出倍率（定格不加速）
    let (moving, holds) = lengths(ed);
    ui.horizontal(|ui| {
        ui.label(theme::muted(ui, tr!("做成長度", "Target length")).font(theme::font(12.0)));
        let resp = ui.add(egui::TextEdit::singleline(&mut ed.speed_target).desired_width(64.0).hint_text("1:00").font(theme::font(13.0)));
        let target = parse_clock(&ed.speed_target);
        let go = Btn::new(tr!("套用", "Apply")).small().enabled(target.is_some() && moving > 0.0).show(ui).clicked() || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
        if go {
            if let Some(t) = target.filter(|t| *t > holds) {
                set_whole(ed, moving / (t - holds));
            }
        }
    });
    let need = parse_clock(&ed.speed_target).map(|t| if t > holds { moving / (t - holds) } else { f64::INFINITY });
    match need {
        Some(n) if n > WHOLE_MAX => hint(ui, &trf!("要加速到 {}× 以上才做得到，最多 {}×", "That needs more than {}×; the maximum is {}×", speed_label(n.min(999.0)), speed_label(WHOLE_MAX))),
        Some(n) if n < 1.0 => hint(ui, tr!("剪好的影片已經比這個短，不用加速", "The edited video is already shorter than this, so no speed-up is needed")),
        _ => {}
    }
    let out = moving / cur + holds;
    ui.label(theme::muted(ui, trf!("剪好 {} → 輸出 {}", "Edited {} → output {}", video_clock(moving + holds), video_clock(out))).font(theme::font(12.0)));
    if cur > 1.0 {
        theme::check(ui, &mut ed.spec.speed_audio, RichText::new(tr!("保留聲音（變快、音調不變）", "Keep sound (faster, same pitch)")).font(theme::font(12.5))).on_hover_text(tr!(
            "不勾的話整支影片沒有原本的聲音（旁白照樣有）。另外設了加速、放慢的片段一樣沒有原本的聲音",
            "If unticked, the whole video has no original sound (narration still plays). Sections with their own speed change have no original sound either"
        ));
        if cur > 2.0 && ed.spec.speed_audio {
            hint(ui, tr!("超過 2 倍時講話很難聽清楚。", "Speech is hard to follow above 2×."));
        }
    }
}

/// 選取的每一段都改成這個速度（Ctrl 多選的一起改），之後取消選取
fn apply(ed: &mut Editor, all: &[(f64, f64)], speed: f64) {
    for &(a, b) in all {
        set_fast(&mut ed.spec.fast, a, b, speed);
    }
    ed.sel = None;
}

/// 「已選取 0:10 – 0:20（0:10）」；多段時「已選取 3 段（共 0:25）」
pub(super) fn selected_text(all: &[(f64, f64)]) -> String {
    match all {
        [(a, b)] => trf!("已選取 {} – {}（{}）", "Selected {} – {} ({})", video_clock(*a), video_clock(*b), video_clock(b - a)),
        _ => trf!("已選取 {} 段（共 {}）", "{} sections selected ({} in total)", all.len(), video_clock(all.iter().map(|(a, b)| b - a).sum())),
    }
}

/// 選取的片段加速、放慢（含自訂倍率），以及已經變速的片段
fn sections(ed: &mut Editor, ui: &mut egui::Ui) {
    let p = theme::pal(ui);
    section(ui, tr!("局部加速、放慢", "Speed up or slow down a section"), false);
    let all = ed.sels();
    if !all.is_empty() {
        egui::Frame::new().fill(Color32::from_rgba_unmultiplied(214, 140, 18, 31)).corner_radius(6).inner_margin(8).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.label(RichText::new(selected_text(&all)).font(theme::font(12.5)));
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                ui.label(theme::muted(ui, tr!("加速", "Faster")).font(theme::font(12.0)));
                for s in FAST_SPEEDS {
                    if Btn::new(format!("{s}×")).small().tooltip(trf!("這段以 {s} 倍速播放（沒有聲音）", "Play this section at {s}× speed (no sound)")).show(ui).clicked() {
                        apply(ed, &all, s as f64);
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                ui.label(theme::muted(ui, tr!("放慢", "Slower")).font(theme::font(12.0)));
                for s in SLOW_SPEEDS {
                    let label = format!("{}×", speed_label(s));
                    if Btn::new(&label).small().tooltip(trf!("這段以 {label} 播放（變長，沒有原本的聲音）", "Play this section at {label} (longer, without the original sound)")).show(ui).clicked()
                    {
                        apply(ed, &all, s);
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(theme::muted(ui, tr!("自訂", "Custom")).font(theme::font(12.0)));
                ui.add(egui::DragValue::new(&mut ed.sel_speed).range(0.25..=16.0).speed(0.05).fixed_decimals(2).suffix("×"));
                let ok = valid_speed(ed.sel_speed);
                if Btn::new(tr!("套用", "Apply")).small().enabled(ok).tooltip(tr!("0.25～0.99 倍放慢、1.01～16 倍加速", "0.25–0.99× slows down, 1.01–16× speeds up")).show(ui).clicked()
                {
                    let speed = ed.sel_speed;
                    apply(ed, &all, speed);
                }
                let overlaps = ed.spec.fast.iter().any(|f| all.iter().any(|&(a, b)| f.start() < b && f.end() > a));
                if overlaps && Btn::new(tr!("原速", "1×")).ghost().small().tooltip(tr!("這段恢復原本的速度", "Restore this section's original speed")).show(ui).clicked() {
                    apply(ed, &all, 1.0);
                }
            });
        });
    } else {
        hint(
            ui,
            tr!(
                "先在時間軸的縮圖上拖過一段，再選倍率。等待、重複的操作可以快轉帶過，快速的操作可以放慢看清楚。",
                "First drag across a section of the timeline thumbnails, then pick a speed. Fast-forward waiting or repetitive steps, or slow down quick ones."
            ),
        );
    }
    if ed.spec.fast.is_empty() {
        return;
    }
    let mute = if ed.entry.media.has_audio == Some(true) { tr!("（無聲）", " (muted)") } else { "" };
    let mut remove = None;
    let mut jump = None;
    for (i, f) in ed.spec.fast.iter().enumerate() {
        ui.horizontal(|ui| {
            let text = format!("{}×{mute}  {} – {}", speed_label(f.speed), video_clock(f.start()), video_clock(f.end()));
            if ui
                .add(egui::Label::new(RichText::new(text).font(theme::mono(12.0)).color(p.accent)).sense(Sense::click()))
                .on_hover_text(tr!("選取這段", "Select this section"))
                .on_hover_cursor(CursorIcon::PointingHand)
                .clicked()
            {
                jump = Some((f.start(), f.end()));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if Btn::new("×").small().ghost().tooltip(tr!("恢復原速", "Back to original speed")).show(ui).clicked() {
                    remove = Some(i);
                }
            });
        });
    }
    if let Some(i) = remove {
        ed.spec.fast.remove(i);
    }
    if let Some((a, b)) = jump {
        ed.seek(a);
        ed.set_sel(Some((a, b)));
    }
    if ed.spec.fast.len() > 1 && Btn::new(tr!("全部恢復原速", "Reset all sections")).small().ghost().show(ui).clicked() {
        ed.spec.fast.clear();
    }
}

/// 定格：停在播放點這一格幾秒（搭配文字標註解說）
fn freezes(ed: &mut Editor, ui: &mut egui::Ui) {
    let p = theme::pal(ui);
    section(ui, tr!("定格", "Freeze frame"), false);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
        ui.label(theme::muted(ui, tr!("在播放點停住", "Hold at playhead")).font(theme::font(12.0)));
        for secs in FREEZE_SECS {
            let label = trf!("{} 秒", "{}s", speed_label(secs));
            if Btn::new(&label)
                .small()
                .tooltip(trf!("影片停在這一格 {label}，再繼續播放（可以加文字標註說明）", "The video holds on this frame for {label}, then continues (add a text annotation to explain)"))
                .show(ui)
                .clicked()
            {
                let at_ms = (ed.now() * 1000.0).round() as u64;
                ed.spec.freezes.retain(|f| f.at_ms.abs_diff(at_ms) >= 50);
                ed.spec.freezes.push(Freeze { at_ms, ms: (secs * 1000.0) as u64 });
                ed.spec.freezes.sort_by_key(|f| f.at_ms);
            }
        }
    });
    if ed.spec.freezes.is_empty() {
        return;
    }
    let mut remove = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        for (i, f) in ed.spec.freezes.iter().enumerate() {
            let text = trf!("定格 {} 停 {} 秒", "Freeze {} for {}s", video_clock(f.at()), speed_label(f.secs()));
            let g = ui.painter().layout_no_wrap(text, theme::mono(12.0), p.accent);
            let (r, _) = ui.allocate_exact_size(vec2(g.size().x + 10.0 + 24.0, 24.0), Sense::hover());
            ui.painter().rect_filled(r, CornerRadius::same(12), p.accent.gamma_multiply(0.14));
            ui.painter().galley(pos2(r.left() + 10.0, r.center().y - g.size().y / 2.0), g, p.accent);
            let x = egui::Rect::from_center_size(pos2(r.right() - 13.0, r.center().y), vec2(20.0, 20.0));
            let xr = ui.interact(x, Id::new(("ed-freeze", i)), Sense::click()).on_hover_text(tr!("拿掉這個定格", "Remove this freeze")).on_hover_cursor(CursorIcon::PointingHand);
            if xr.hovered() {
                ui.painter().circle_filled(x.center(), 10.0, p.accent.gamma_multiply(0.2));
            }
            ui.painter().text(x.center(), Align2::CENTER_CENTER, "×", theme::font(14.0), p.accent);
            if xr.clicked() {
                remove = Some(i);
            }
        }
    });
    if let Some(i) = remove {
        ed.spec.freezes.remove(i);
    }
}
