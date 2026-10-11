//! 右側面板：時間（頭尾、刪除的片段）、畫面裁切、標註（工具、屬性、清單）；截圖另有「輸出」（外框、陰影、大小）。

use super::shot::Act;
use super::stage::paint_tool_icon;
use super::{ann_color, parse_time, Editor, Tab, Tool};
use crate::ui::dialogs::file_name;
use crate::ui::theme::{self, segmented, switch, Btn, Icon};
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, CornerRadius, CursorIcon, Id, Layout, RichText, Sense, Stroke};
use screenrecorder_core::annotate::{self, AnnKind, Shape, COLORS};
use screenrecorder_core::edit::{normalize_crop, normalize_ranges, set_fast, CropInput, FAST_SPEEDS};
use screenrecorder_core::format::video_clock;
use screenrecorder_core::idle;
use screenrecorder_core::picture;
use screenrecorder_core::shot_edit::BACKGROUNDS;
use screenrecorder_core::video_frame::{self, VideoFrame};
use screenrecorder_core::zoom;
use screenrecorder_core::{tr, trf};

pub fn show(ed: &mut Editor, ui: &mut egui::Ui, ctx: &egui::Context, toast: &mut Option<(String, bool)>) {
    tabs(ed, ui);
    ui.add_space(4.0);
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.set_width(ui.available_width() - 6.0);
        ui.spacing_mut().item_spacing = vec2(8.0, 10.0);
        match ed.tab {
            Tab::Time => time_panel(ed, ui, toast),
            Tab::Crop => crop_panel(ed, ui),
            Tab::Ann => ann_panel(ed, ui, ctx),
            Tab::Output => super::shot::output_panel(ed, ui, ctx),
        }
    });
}

fn tabs(ed: &mut Editor, ui: &mut egui::Ui) {
    let p = theme::pal(ui);
    let count = if ed.anns.is_empty() { String::new() } else { ed.anns.len().to_string() };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let tabs: [(Tab, &str); 3] = if ed.is_shot() {
            [(Tab::Ann, tr!("標註", "Annotate")), (Tab::Crop, tr!("裁切與旋轉", "Crop & rotate")), (Tab::Output, tr!("輸出", "Output"))]
        } else {
            [(Tab::Time, tr!("時間", "Trim")), (Tab::Crop, tr!("畫面裁切", "Crop")), (Tab::Ann, tr!("標註", "Annotate"))]
        };
        for &(tab, label) in &tabs {
            let on = ed.tab == tab;
            let f = if on { theme::font_bold(13.0) } else { theme::font(13.0) };
            let g = ui.painter().layout_no_wrap(label.to_string(), f, if on { p.text } else { p.muted });
            let extra = if tab == Tab::Ann && !count.is_empty() {
                ui.painter().layout_no_wrap(count.clone(), theme::font_bold(13.0), p.accent)
            } else {
                ui.painter().layout_no_wrap(String::new(), theme::font(13.0), p.accent)
            };
            let w = g.size().x + if extra.size().x > 0.0 { extra.size().x + 4.0 } else { 0.0 } + 28.0;
            let (r, resp) = ui.allocate_exact_size(vec2(w, 32.0), Sense::click());
            let y = r.center().y - g.size().y / 2.0;
            let gw = g.size().x;
            ui.painter().galley(pos2(r.left() + 14.0, y), g, p.text);
            if extra.size().x > 0.0 {
                ui.painter().galley(pos2(r.left() + 18.0 + gw, y), extra, p.accent);
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

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(theme::muted(ui, text).font(theme::font(12.0)));
}

fn time_panel(ed: &mut Editor, ui: &mut egui::Ui, toast: &mut Option<(String, bool)>) {
    let p = theme::pal(ui);
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
    if let Some((a, b)) = ed.sel {
        egui::Frame::new().fill(Color32::from_rgba_unmultiplied(214, 140, 18, 31)).corner_radius(6).inner_margin(8).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.label(RichText::new(trf!("已選取 {} – {}（{}）", "Selected {} – {} ({})", video_clock(a), video_clock(b), video_clock(b - a))).font(theme::font(12.5)));
            ui.horizontal(|ui| {
                if Btn::new(tr!("刪除這段", "Delete section")).danger().small().tooltip(tr!("快速鍵 Delete", "Shortcut: Delete")).show(ui).clicked() {
                    ed.delete_selection(toast);
                }
                if Btn::new(tr!("取消選取", "Deselect")).ghost().small().tooltip(tr!("快速鍵 Esc", "Shortcut: Esc")).show(ui).clicked() {
                    ed.sel = None;
                }
            });
            // 局部加速：等待、重複的操作快轉帶過
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(theme::muted(ui, tr!("這段加速", "Speed up")).font(theme::font(12.0)));
                for s in FAST_SPEEDS {
                    if Btn::new(format!("{s}×")).small().tooltip(trf!("這段以 {s} 倍速播放（沒有聲音）", "Play this section at {s}× speed (no sound)")).show(ui).clicked() {
                        set_fast(&mut ed.spec.fast, a, b, s);
                        ed.sel = None;
                    }
                }
                let overlaps = ed.spec.fast.iter().any(|f| f.start() < b && f.end() > a);
                if overlaps && Btn::new(tr!("原速", "1×")).ghost().small().tooltip(tr!("這段恢復原本的速度", "Restore this section's original speed")).show(ui).clicked() {
                    set_fast(&mut ed.spec.fast, a, b, 1);
                    ed.sel = None;
                }
            });
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
    }
    // 加速的片段
    if !ed.spec.fast.is_empty() {
        let mut remove = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            for (i, f) in ed.spec.fast.iter().enumerate() {
                let g = ui.painter().layout_no_wrap(format!("{}× {} – {}", f.speed, video_clock(f.start()), video_clock(f.end())), theme::mono(12.0), p.accent);
                let (r, _) = ui.allocate_exact_size(vec2(g.size().x + 10.0 + 24.0, 24.0), Sense::hover());
                ui.painter().rect_filled(r, CornerRadius::same(12), p.accent.gamma_multiply(0.14));
                ui.painter().galley(pos2(r.left() + 10.0, r.center().y - g.size().y / 2.0), g, p.accent);
                let x = egui::Rect::from_center_size(pos2(r.right() - 13.0, r.center().y), vec2(20.0, 20.0));
                let xr = ui.interact(x, Id::new(("ed-fast", i)), Sense::click()).on_hover_text(tr!("恢復原速", "Back to original speed")).on_hover_cursor(CursorIcon::PointingHand);
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
            ed.spec.fast.remove(i);
        }
    }
    hint(
        ui,
        tr!(
            "在時間軸上拖過一段即可選取，按「刪除這段」或 Delete 鍵刪除，或讓這段加速。",
            "Drag across the timeline to select a section, then press “Delete section” or the Delete key to remove it, or speed it up."
        ),
    );
    // 自動找出畫面不動又沒聲音的片段
    let busy = ed.finding_idle;
    let tip = if ed.entry.media.has_audio == Some(true) {
        trf!(
            "分析整支影片，找出畫面不動、也沒有聲音超過 {} 秒的地方，直接刪掉（可按 Ctrl+Z 復原）",
            "Analyze the whole video and remove places where the picture is still and silent for more than {} s (press Ctrl+Z to undo)",
            idle::MIN_IDLE
        )
    } else {
        trf!(
            "分析整支影片，找出畫面不動超過 {} 秒的地方，直接刪掉（可按 Ctrl+Z 復原）",
            "Analyze the whole video and remove places where the picture is still for more than {} s (press Ctrl+Z to undo)",
            idle::MIN_IDLE
        )
    };
    if Btn::new(if busy { tr!("正在分析…", "Analyzing…") } else { tr!("自動剪掉沒動靜的片段", "Auto-cut idle parts") }).small().enabled(!busy && ed.duration > 0.0).tooltip(tip).show(ui).clicked()
    {
        ed.pending = Some(super::shot::Act::FindIdle);
    }
    audio_panel(ed, ui);
}

/// 聲音處理（輸出時套用）
fn audio_panel(ed: &mut Editor, ui: &mut egui::Ui) {
    let p = theme::pal(ui);
    let has = ed.entry.media.has_audio == Some(true);
    ui.add_space(4.0);
    let r = ui.cursor();
    ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0, p.border));
    ui.add_space(6.0);
    ui.label(RichText::new(tr!("聲音", "Audio")).font(theme::font_bold(13.5)));
    if !has {
        return hint(ui, tr!("這支影片沒有聲音。", "This video has no audio."));
    }
    let fx = &mut ed.spec.audio;
    switch(ui, &mut fx.mute, tr!("不要聲音", "Remove audio"), true).on_hover_text(tr!("輸出的影片沒有聲音", "The output video has no sound"));
    ui.add_enabled_ui(!fx.mute, |ui| {
        switch(ui, &mut fx.denoise, tr!("降噪", "Noise reduction"), true).on_hover_text(tr!("減少風扇、冷氣等持續的背景雜音", "Reduce constant background noise such as fans or air conditioning"));
        switch(ui, &mut fx.normalize, tr!("音量平衡", "Normalize loudness"), true)
            .on_hover_text(tr!("忽大忽小的音量變平均，整體調到適合聆聽的大小", "Even out loud and quiet parts and set a comfortable overall volume"));
    });
    hint(ui, tr!("預覽時聽到的是原本的聲音，輸出的影片才會套用。", "The preview plays the original audio; these settings apply to the output video only."));
}

/// 分頁裡的小節標題（第一節不畫分隔線）
fn section(ui: &mut egui::Ui, title: &str, first: bool) {
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
    if !shot {
        zoom_panel(ed, ui);
        frame_panel(ed, ui);
    }
}

/// 背景與圓角：影片縮小放在背景中間（輸出大小不變），四角變圓、有陰影
fn frame_panel(ed: &mut Editor, ui: &mut egui::Ui) {
    let p = theme::pal(ui);
    ui.add_space(4.0);
    let r = ui.cursor();
    ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0, p.border));
    ui.add_space(6.0);
    ui.label(RichText::new(tr!("背景與圓角", "Background and rounded corners")).font(theme::font_bold(13.5)));
    let mut on = ed.spec.frame.is_some();
    if switch(ui, &mut on, tr!("放在背景上", "Place on a background"), ed.vw > 0.0)
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
fn zoom_panel(ed: &mut Editor, ui: &mut egui::Ui) {
    let p = theme::pal(ui);
    ui.add_space(4.0);
    let r = ui.cursor();
    ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0, p.border));
    ui.add_space(6.0);
    ui.label(RichText::new(tr!("跟著點擊放大", "Zoom on clicks")).font(theme::font_bold(13.5)));
    if ed.clicks.is_empty() {
        return hint(
            ui,
            tr!("這支錄影沒有記下滑鼠點擊（3.1 版以後在 Windows 上錄的影片才有）。", "This recording has no saved mouse clicks (only videos recorded on Windows with version 3.1 or later have them)."),
        );
    }
    let can = !ed.crop_on;
    let mut on = ed.zoom > 1.0;
    let n = ed.clicks.len();
    let label = if screenrecorder_core::i18n::is_en() { format!("Zoom in on clicks ({n} click{})", if n == 1 { "" } else { "s" }) } else { format!("點擊時放大（{n} 次點擊）") };
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
    }
    hint(
        ui,
        if can {
            tr!(
                "點擊前畫面慢慢放大到點擊的地方，連續點擊時跟著移動，停下來後拉回全畫面。按「預覽結果」可以看到效果。",
                "Before each click the view slowly zooms in on it, follows along during a series of clicks, then zooms back out. Press “Preview result” to see the effect."
            )
        } else {
            tr!("裁切畫面時不能同時使用。", "Can't be used together with cropping.")
        },
    );
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
    let p = theme::pal(ui);
    let can = ed.vw > 0.0;
    let shot = ed.is_shot();

    // 加上標註：文字、箭頭、框線…、圖片
    section(ui, tr!("加上標註", "Add annotations"), true);
    tool_grid(ed, ui, &MARK_TOOLS, can, true);
    if ed.tool == Some(Tool::Emoji) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
            for i in 0..ed.emoji_tex.len() {
                let (r, resp) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::click());
                let on = ed.emoji == i;
                if on || resp.hovered() {
                    ui.painter().rect(r, CornerRadius::same(6), p.surface2, if on { Stroke::new(2.0, p.accent) } else { Stroke::NONE }, egui::StrokeKind::Inside);
                }
                let t = &ed.emoji_tex[i];
                let sz = t.size_vec2();
                let k = 24.0 / sz.y.max(1.0);
                ui.painter().image(t.id(), egui::Rect::from_center_size(r.center(), sz * k), egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    ed.emoji = i;
                }
            }
        });
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
            Some(Tool::Ann(AnnKind::Step)) => {
                trf!("在{w}上依序點擊，放置編號 1、2、3…；完成後按 Esc 或再按一次「編號」。", "Click on the {w} in order to place numbers 1, 2, 3…; when done, press Esc or click “Number” again.")
            }
            Some(Tool::Emoji) => trf!(
                "先在上面選表情符號，再在{w}上點擊放置（可連續放）；完成後按 Esc 或再按一次「表情符號」。",
                "Pick an emoji above, then click on the {w} to place it (you can place several); when done, press Esc or click “Emoji” again."
            ),
            Some(Tool::Ann(AnnKind::Text)) => trf!("在{w}上點一下放置。按 Esc 取消。", "Click on the {w} to place it. Press Esc to cancel."),
            Some(Tool::Ann(AnnKind::Pen)) => trf!(
                "在{w}上按住拖曳手繪（可以連續畫好幾筆）；完成後按 Esc 或再按一次「畫筆」。",
                "Drag on the {w} to draw freehand (you can draw several strokes); when done, press Esc or click “Pen” again."
            ),
            Some(Tool::Ann(AnnKind::Magnify)) => trf!(
                "在{w}上按住拖曳框出要放大的地方，圓裡會顯示中心附近放大的樣子。按 Esc 取消。",
                "Drag on the {w} to select the area to magnify; the circle shows a zoomed-in view of its center. Press Esc to cancel."
            ),
            Some(_) => trf!("在{w}上按住拖曳放置。按 Esc 取消。", "Drag on the {w} to place it. Press Esc to cancel."),
            None if ed.selected().is_some() => String::new(),
            None if shot => tr!("① 選工具 ② 在圖上點一下或拖曳放置。Ctrl+Z 復原、Ctrl+Y 重做。", "① Pick a tool ② Click or drag on the image to place it. Ctrl+Z to undo, Ctrl+Y to redo.").into(),
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
    // 影片：字幕（自動產生或匯入 SRT）
    if !shot && can && ed.tool.is_none() && ed.selected().is_none() {
        super::subs::panel(ed, ui);
    }
    tool_style(ed, ui);

    if ed.selected().is_some() {
        props(ed, ui, ctx);
    }
    ann_list(ed, ui);
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
fn style_controls(ui: &mut egui::Ui, kind: AnnKind, color: &mut String, size: &mut f64, k: f64) -> (bool, bool) {
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
        if ui.add(egui::Slider::new(&mut v, (lo * k).round()..=(hi * k).round()).step_by(1.0).fixed_decimals(0)).changed() {
            *size = v;
            sc = true;
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
    let vh = if ed.vh > 0.0 { ed.vh } else { 1080.0 };
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
        let (cc, sc) = style_controls(ui, kind, &mut color, &mut size, vh / 1080.0);
        if cc || sc {
            super::remember_style(kind, &color, size, vh);
        }
        ui.label(theme::muted(ui, tr!("接下來放的都用這個設定；放好的可以點選後再改。", "New annotations use these settings; select a placed one to change it.")).font(theme::font(12.0)));
    });
}

/// 選取的標註的屬性（改一份副本，最後寫回）
fn props(ed: &mut Editor, ui: &mut egui::Ui, ctx: &egui::Context) {
    let p = theme::pal(ui);
    let keep = ed.keep();
    let Some(a) = ed.selected().cloned() else {
        return;
    };
    let mut cur = a.clone();
    let (dur, now) = (ed.duration, ed.now());
    let k = (if ed.vh > 0.0 { ed.vh } else { 1080.0 }) / 1080.0;
    let mut delete = false;
    let mut apply_all = false;
    egui::Frame::new().fill(p.surface2).corner_radius(theme::RADIUS_SM).inner_margin(10).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(annotate::label(&a)).font(theme::font_bold(13.5)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if Btn::new(tr!("刪除", "Delete")).danger().small().tooltip(tr!("快速鍵 Delete", "Shortcut: Delete")).show(ui).clicked() {
                    delete = true;
                }
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
        let is_text = cur.kind == AnnKind::Text;
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
        if cur.kind == AnnKind::Magnify {
            ui.horizontal(|ui| {
                ui.label(theme::muted(ui, tr!("倍率", "Zoom")).font(theme::font(12.0)));
                let mut z = cur.size / 100.0;
                ui.spacing_mut().slider_width = ui.available_width() - 60.0;
                if ui.add(egui::Slider::new(&mut z, 1.5..=4.0).step_by(0.25).fixed_decimals(2).suffix("×")).changed() {
                    cur.size = (z * 100.0).round();
                }
            });
        } else if cur.kind == AnnKind::Image {
            ui.horizontal(|ui| {
                ui.label(theme::muted(ui, tr!("不透明度", "Opacity")).font(theme::font(12.0)));
                let mut v = cur.size.clamp(5.0, 100.0);
                ui.spacing_mut().slider_width = ui.available_width() - 60.0;
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
            let (cc, sc) = style_controls(ui, cur.kind, &mut cur.color, &mut cur.size, k);
            if sc {
                // 文字、編號放大時以中心為準
                let (cx, cy) = (cur.x + cur.w / 2.0, cur.y + cur.h / 2.0);
                annotate::measure(&mut cur);
                if matches!(cur.kind, AnnKind::Text | AnnKind::Step) {
                    cur.x = cx - cur.w / 2.0;
                    cur.y = cy - cur.h / 2.0;
                }
            }
            if cc || sc {
                // 之後新放的同類標註沿用
                super::remember_style(cur.kind, &cur.color, cur.size, ed.vh);
            }
            if is_text {
                switch(ui, &mut cur.bg, tr!("深色底", "Dark background"), true);
            }
        }

        // 同類的標註一次改成一樣（例如所有編號都改成綠色、一樣大）
        let same = ed.anns.iter().filter(|o| o.kind == cur.kind).count();
        // 表情符號不算（顏色、大小是另外選的）
        let emoji = cur.kind == AnnKind::Text && cur.text.as_deref().is_some_and(super::is_emoji_text);
        let all = if screenrecorder_core::i18n::is_en() {
            format!("Apply to all {same} {} annotations", cur.kind.label().to_lowercase())
        } else {
            format!("全部 {same} 個{}都改成這樣", cur.kind.label())
        };
        if same > 1
            && !emoji
            && Btn::new(all)
                .ghost()
                .small()
                .tooltip(tr!("顏色、大小（以及形狀、底色）套用到所有同類的標註", "Apply the color and size (and shape and background) to all annotations of this type"))
                .show(ui)
                .clicked()
        {
            apply_all = true;
        }
        // 角度（箭頭不用：兩端本來就能指向任何方向）
        if cur.kind.rotatable() {
            ui.horizontal(|ui| {
                ui.label(theme::muted(ui, tr!("角度", "Angle")).font(theme::font(12.0)));
                let mut deg = cur.rot;
                ui.spacing_mut().slider_width = ui.available_width() - 120.0;
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

        // 出現時間（截圖沒有）
        if ed.is_shot() {
            return;
        }
        ui.horizontal(|ui| {
            ui.label(theme::muted(ui, tr!("出現", "From")).font(theme::font(12.0)));
            for (i, sep) in [(0usize, Some(tr!("到", "to"))), (1, None)] {
                let id = Id::new(("ed-time", i));
                let focused = ui.memory(|m| m.has_focus(id));
                if !focused {
                    ed.time_text[i] = video_clock(if i == 0 { cur.start } else { cur.end });
                }
                let resp = ui.add(egui::TextEdit::singleline(&mut ed.time_text[i]).id(id).desired_width(76.0).font(theme::mono(13.0)));
                if resp.lost_focus() {
                    if let Some(v) = parse_time(&ed.time_text[i]) {
                        let t = v.clamp(0.0, dur);
                        if i == 0 {
                            cur.start = t.min(cur.end - 0.1);
                        } else {
                            cur.end = t.max(cur.start + 0.1);
                        }
                    }
                }
                if let Some(s) = sep {
                    ui.label(theme::muted(ui, s).font(theme::font(12.0)));
                }
            }
        });
        ui.horizontal(|ui| {
            if Btn::new(tr!("從目前時間開始", "Start at playhead")).ghost().small().tooltip(tr!("從播放頭的時間開始", "Start at the playhead position")).show(ui).clicked() {
                cur.start = now.min(cur.end - 0.1);
            }
            if Btn::new(tr!("到目前時間結束", "End at playhead")).ghost().small().tooltip(tr!("到播放頭的時間結束", "End at the playhead position")).show(ui).clicked() {
                cur.end = now.max(cur.start + 0.1);
            }
        });
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
    if apply_all {
        for o in ed.anns.iter_mut().filter(|o| o.kind == cur.kind && o.id != cur.id) {
            // 表情符號不改（它們的顏色、大小是另外選的）
            if o.kind == AnnKind::Text && o.text.as_deref().is_some_and(super::is_emoji_text) {
                continue;
            }
            let (cx, cy) = (o.x + o.w / 2.0, o.y + o.h / 2.0);
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
                o.y = cy - o.h / 2.0;
            }
        }
    }
}

/// 標註清單（依出現時間）：點一下選取並跳到它出現的時間
fn ann_list(ed: &mut Editor, ui: &mut egui::Ui) {
    if ed.anns.is_empty() {
        return;
    }
    let p = theme::pal(ui);
    let keep = ed.keep();
    let mut list: Vec<(u64, f64, f64, String, Color32, bool)> = ed.anns.iter().map(|a| (a.id, a.start, a.end, annotate::label(a), ann_color(a), !Editor::in_output(a, &keep))).collect();
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
        let np = painter.with_clip_rect(name_rect);
        let ng = np.layout_no_wrap(name, theme::font(12.5), if gone { p.muted } else { p.text });
        let ny = r.center().y - ng.size().y / 2.0;
        let nw = ng.size().x.min(name_rect.width());
        np.galley(pos2(name_rect.left(), ny), ng, p.text);
        if gone {
            np.hline(name_rect.left()..=name_rect.left() + nw, r.center().y, Stroke::new(1.0, p.muted));
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
