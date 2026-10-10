//! 右側面板：時間（頭尾、刪除的片段）、畫面裁切、標註（工具、屬性、清單）；截圖另有「輸出」（外框、陰影、大小）。

use super::shot::Act;
use super::stage::paint_tool_icon;
use super::{ann_color, parse_time, Editor, Tab, Tool};
use crate::ui::dialogs::file_name;
use crate::ui::theme::{self, segmented, switch, Btn, Icon};
use eframe::egui::{self, pos2, vec2, Align, Align2, Color32, CornerRadius, CursorIcon, Id, Layout, RichText, Sense, Stroke};
use screenrecorder_core::annotate::{self, AnnKind, Shape, COLORS};
use screenrecorder_core::edit::{normalize_ranges, set_fast, CropInput, FAST_SPEEDS};
use screenrecorder_core::format::video_clock;
use screenrecorder_core::idle;
use screenrecorder_core::picture;
use screenrecorder_core::zoom;

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
        let tabs: &[(Tab, &str)] =
            if ed.is_shot() { &[(Tab::Ann, "標註"), (Tab::Crop, "裁切與旋轉"), (Tab::Output, "輸出")] } else { &[(Tab::Time, "時間"), (Tab::Crop, "畫面裁切"), (Tab::Ann, "標註")] };
        for &(tab, label) in tabs {
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
        if Btn::new("設為開頭").small().tooltip("快捷鍵 I").show(ui).clicked() {
            let t = ed.now();
            ed.set_start(t);
        }
        if Btn::new("設為結尾").small().tooltip("快捷鍵 O").show(ui).clicked() {
            let t = ed.now();
            ed.set_end(t);
        }
    });
    ui.label(theme::muted(ui, format!("開頭 {}　結尾 {}", video_clock(ed.spec.start), video_clock(ed.spec.end))));
    if let Some((a, b)) = ed.sel {
        egui::Frame::new().fill(Color32::from_rgba_unmultiplied(214, 140, 18, 31)).corner_radius(6).inner_margin(8).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 6.0;
            ui.label(RichText::new(format!("已選取 {} – {}（{}）", video_clock(a), video_clock(b), video_clock(b - a))).font(theme::font(12.5)));
            ui.horizontal(|ui| {
                if Btn::new("刪除這段").danger().small().tooltip("快捷鍵 Delete").show(ui).clicked() {
                    ed.delete_selection(toast);
                }
                if Btn::new("取消選取").ghost().small().tooltip("快捷鍵 Esc").show(ui).clicked() {
                    ed.sel = None;
                }
            });
            // 局部加速：等待、重複的操作快轉帶過
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(theme::muted(ui, "這段加速").font(theme::font(12.0)));
                for s in FAST_SPEEDS {
                    if Btn::new(format!("{s}×")).small().tooltip(format!("這段以 {s} 倍速播放（沒有聲音）")).show(ui).clicked() {
                        set_fast(&mut ed.spec.fast, a, b, s);
                        ed.sel = None;
                    }
                }
                let overlaps = ed.spec.fast.iter().any(|f| f.start() < b && f.end() > a);
                if overlaps && Btn::new("原速").ghost().small().tooltip("這段恢復原本的速度").show(ui).clicked() {
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
                let g = ui.painter().layout_no_wrap(format!("刪除 {} – {}", video_clock(*a), video_clock(*b)), theme::mono(12.0), p.rec);
                let (r, _) = ui.allocate_exact_size(vec2(g.size().x + 10.0 + 24.0, 24.0), Sense::hover());
                ui.painter().rect_filled(r, CornerRadius::same(12), p.rec_soft);
                ui.painter().galley(pos2(r.left() + 10.0, r.center().y - g.size().y / 2.0), g, p.rec);
                let x = egui::Rect::from_center_size(pos2(r.right() - 13.0, r.center().y), vec2(20.0, 20.0));
                let xr = ui.interact(x, Id::new(("ed-restore", i)), Sense::click()).on_hover_text("還原這段").on_hover_cursor(CursorIcon::PointingHand);
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
                let xr = ui.interact(x, Id::new(("ed-fast", i)), Sense::click()).on_hover_text("恢復原速").on_hover_cursor(CursorIcon::PointingHand);
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
    hint(ui, "在時間軸上拖過一段即可選取，按「刪除這段」或 Delete 鍵刪除，或讓這段加速。");
    // 自動找出畫面不動又沒聲音的片段
    let busy = ed.finding_idle;
    let tip = if ed.entry.media.has_audio == Some(true) {
        format!("分析整支影片，找出畫面不動、也沒有聲音超過 {} 秒的地方，直接刪掉（可按 Ctrl+Z 復原）", idle::MIN_IDLE)
    } else {
        format!("分析整支影片，找出畫面不動超過 {} 秒的地方，直接刪掉（可按 Ctrl+Z 復原）", idle::MIN_IDLE)
    };
    if Btn::new(if busy { "正在分析…" } else { "自動剪掉沒動靜的片段" }).small().enabled(!busy && ed.duration > 0.0).tooltip(tip).show(ui).clicked() {
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
    ui.label(RichText::new("聲音").font(theme::font_bold(13.5)));
    if !has {
        return hint(ui, "這支影片沒有聲音。");
    }
    let fx = &mut ed.spec.audio;
    switch(ui, &mut fx.mute, "不要聲音", true).on_hover_text("輸出的影片沒有聲音");
    ui.add_enabled_ui(!fx.mute, |ui| {
        switch(ui, &mut fx.denoise, "降噪", true).on_hover_text("減少風扇、冷氣等持續的背景雜音");
        switch(ui, &mut fx.normalize, "音量平衡", true).on_hover_text("忽大忽小的音量變平均，整體調到適合聆聽的大小");
    });
    hint(ui, "預覽時聽到的是原本的聲音，輸出的影片才會套用。");
}

fn crop_panel(ed: &mut Editor, ui: &mut egui::Ui) {
    if ed.is_shot() {
        ui.horizontal(|ui| {
            ui.label(theme::muted(ui, "旋轉").font(theme::font(12.0)));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| super::shot::rotate_buttons(ed, ui));
        });
        ui.add_space(4.0);
    }
    let can = ed.vw > 0.0;
    let mut on = ed.crop_on;
    switch(ui, &mut on, "裁切畫面", can);
    if on != ed.crop_on {
        ed.crop_on = on;
        if on && ed.spec.crop.is_none() && can {
            let ev = |v: f64| (v / 2.0).round() * 2.0;
            ed.spec.crop = Some(CropInput { x: ev(ed.vw * 0.05), y: ev(ed.vh * 0.05), width: ev(ed.vw * 0.8), height: ev(ed.vh * 0.8) });
        }
    }
    let c = ed.crop_rect();
    let mut v = [c.x, c.y, c.width, c.height];
    let mut changed = false;
    ui.add_enabled_ui(ed.crop_on && can, |ui| {
        egui::Grid::new("ed-crop").num_columns(2).spacing(vec2(12.0, 8.0)).show(ui, |ui| {
            let labels = ["X", "Y", "寬度", "高度"];
            let maxes = [ed.vw - 16.0, ed.vh - 16.0, ed.vw, ed.vh];
            for (i, l) in labels.iter().enumerate() {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(theme::muted(ui, *l).font(theme::font(12.0)));
                    let min = if i >= 2 { 16.0 } else { 0.0 };
                    if ui.add_sized(vec2(118.0, 26.0), egui::DragValue::new(&mut v[i]).speed(2.0).range(min..=maxes[i].max(min)).fixed_decimals(0)).changed() {
                        changed = true;
                    }
                });
                if i % 2 == 1 {
                    ui.end_row();
                }
            }
        });
    });
    if changed {
        ed.set_crop(CropInput { x: v[0], y: v[1], width: v[2], height: v[3] });
    }
    let text = if can { format!("開啟後可直接在{}上拖曳框選要保留的區域。", ed.what()) } else { format!("無法讀取{}尺寸，不能裁切。", ed.what()) };
    hint(ui, &text);
    if !ed.is_shot() {
        zoom_panel(ed, ui);
    }
}

/// 跟著點擊放大：錄影時記下的點擊，輸出時放大到點擊的地方
fn zoom_panel(ed: &mut Editor, ui: &mut egui::Ui) {
    let p = theme::pal(ui);
    ui.add_space(4.0);
    let r = ui.cursor();
    ui.painter().hline(r.x_range(), r.top(), Stroke::new(1.0, p.border));
    ui.add_space(6.0);
    ui.label(RichText::new("跟著點擊放大").font(theme::font_bold(13.5)));
    if ed.clicks.is_empty() {
        return hint(ui, "這支錄影沒有記下滑鼠點擊（3.1 版以後在 Windows 上錄的影片才有）。");
    }
    let can = !ed.crop_on;
    let mut on = ed.zoom > 1.0;
    if switch(ui, &mut on, format!("點擊時放大（{} 次點擊）", ed.clicks.len()), can).changed() {
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
            "點擊前畫面慢慢放大到點擊的地方，連續點擊時跟著移動，停下來後拉回全畫面。按「預覽結果」可以看到效果。"
        } else {
            "裁切畫面時不能同時使用。"
        },
    );
}

const TOOLS: [(Tool, &str); 12] = [
    (Tool::Ann(AnnKind::Text), "文字"),
    (Tool::Emoji, "表情"),
    (Tool::Ann(AnnKind::Arrow), "箭頭"),
    (Tool::Ann(AnnKind::Rect), "方框"),
    (Tool::Ann(AnnKind::Ellipse), "圓框"),
    (Tool::Ann(AnnKind::Highlight), "螢光筆"),
    (Tool::Ann(AnnKind::Pen), "畫筆"),
    (Tool::Ann(AnnKind::Step), "編號"),
    (Tool::Ann(AnnKind::Mosaic), "馬賽克"),
    (Tool::Ann(AnnKind::Blur), "模糊"),
    (Tool::Ann(AnnKind::Magnify), "放大鏡"),
    (Tool::Ann(AnnKind::Spotlight), "聚光燈"),
];

fn ann_panel(ed: &mut Editor, ui: &mut egui::Ui, ctx: &egui::Context) {
    let p = theme::pal(ui);
    let can = ed.vw > 0.0;

    // 工具
    let gap = 6.0;
    let tw = ((ui.available_width() - gap * 2.0) / 3.0).floor();
    // 放大鏡只能用在截圖
    let shot = ed.is_shot();
    let tools: Vec<(Tool, &str)> = TOOLS.iter().copied().filter(|(t, _)| shot || !t.kind().image_only()).collect();
    for row in tools.chunks(3) {
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
        });
    }
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
    if can {
        picture_button(ed, ui);
    }
    let w = ed.what();
    let hint_text = if !can {
        format!("無法讀取{w}尺寸，不能加上標註。")
    } else {
        match ed.tool {
            Some(Tool::Ann(AnnKind::Step)) => format!("在{w}上依序點擊，放置編號 1、2、3…；完成後按 Esc 或再按一次「編號」。"),
            Some(Tool::Emoji) => format!("先在上面選表情，再在{w}上點擊放置（可連續放）；完成後按 Esc 或再按一次「表情」。"),
            Some(Tool::Ann(AnnKind::Text)) => format!("在{w}上點一下放置。按 Esc 取消。"),
            Some(Tool::Ann(AnnKind::Pen)) => format!("在{w}上按住拖曳手繪（可以連續畫好幾筆）；完成後按 Esc 或再按一次「畫筆」。"),
            Some(Tool::Ann(AnnKind::Magnify)) => format!("在{w}上按住拖曳框出要放大的地方，圓裡會顯示中心附近放大的樣子。按 Esc 取消。"),
            Some(_) => format!("在{w}上按住拖曳放置。按 Esc 取消。"),
            None if ed.selected().is_some() => String::new(),
            None if shot => "① 選工具 ② 在圖上點一下或拖曳放置。Ctrl+Z 復原、Ctrl+Y 重做。".into(),
            None => "① 選工具 ② 在影片上點一下或拖曳放置。標註從目前位置起出現 3 秒，可在時間軸下方的標註軌拖曳調整。".into(),
        }
    };
    if !hint_text.is_empty() {
        hint(ui, &hint_text);
    }
    // 截圖：用文字辨識找出個資，自動打上馬賽克
    if shot && can && ed.tool.is_none() && ed.selected().is_none() {
        let busy = ed.finding_pii;
        let label = if busy { "正在找個資…" } else { "自動遮個資" };
        let resp = Btn::new(label).small().enabled(!busy).tooltip("用文字辨識找出圖裡的 Email、電話、身分證字號、信用卡號，自動打上馬賽克（可再個別調整或刪除）").show(ui);
        if resp.clicked() {
            ed.pending = Some(super::shot::Act::FindPii);
        }
    }
    tool_style(ed, ui);

    if ed.selected().is_some() {
        props(ed, ui, ctx);
    }
    ann_list(ed, ui);
}

/// 加上圖片 / Logo：選檔、上次用的圖、剪貼簿的圖（也可以直接把圖片檔拖曳進來）
fn picture_button(ed: &mut Editor, ui: &mut egui::Ui) {
    let last = ed.last_picture.clone();
    let has_last = !last.is_empty() && std::path::Path::new(&last).is_file();
    let tip = format!("加上圖片：Logo、浮水印、商品照…（可以調整大小、透明度{}）\n也可以直接把圖片檔拖曳到這個視窗", if ed.is_shot() { "" } else { "、出現時間" });
    let resp = Btn::new(if ed.is_shot() { "加上圖片" } else { "加上圖片／Logo" }).icon(Icon::Image).small().min_width(ui.available_width()).tooltip(tip).show(ui);
    egui::Popup::menu(&resp).show(|ui| {
        ui.set_min_width(220.0);
        if ui.button("選擇圖片檔…").clicked() {
            ed.pending = Some(Act::PickPicture { replace: false });
        }
        if has_last && ui.button(format!("上次的圖片「{}」", file_name(&last))).on_hover_text(last.as_str()).clicked() {
            ed.pending = Some(Act::LastPicture);
        }
        if ui.button("貼上剪貼簿裡的圖片").clicked() {
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
            if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(format!("顏色 {c}")).clicked() && color != c {
                *color = c.to_string();
                cc = true;
            }
        }
    });
    let label = match kind {
        AnnKind::Text => "字級",
        AnnKind::Step => "大小",
        AnnKind::Pen => "粗細",
        _ => "線寬",
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
            RichText::new(format!(
                "{}的顏色與{}",
                kind.label(),
                if kind == AnnKind::Pen {
                    "粗細"
                } else if kind == AnnKind::Text {
                    "字級"
                } else if kind == AnnKind::Step {
                    "大小"
                } else {
                    "線寬"
                }
            ))
            .font(theme::font_bold(13.0)),
        );
        let (cc, sc) = style_controls(ui, kind, &mut color, &mut size, vh / 1080.0);
        if cc || sc {
            super::remember_style(kind, &color, size, vh);
        }
        ui.label(theme::muted(ui, "接下來放的都用這個設定；放好的可以點選後再改。").font(theme::font(12.0)));
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
                if Btn::new("刪除").danger().small().tooltip("快捷鍵 Delete").show(ui).clicked() {
                    delete = true;
                }
            });
        });
        if !Editor::in_output(&a, &keep) {
            egui::Frame::new().fill(p.warn_soft).corner_radius(6).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("這個標註在刪除的片段中，輸出的影片看不到。請在標註軌把它拖到保留的部分。").font(theme::font(12.0)).color(p.warn));
            });
        }
        let is_text = cur.kind == AnnKind::Text;
        if is_text {
            ui.label(theme::muted(ui, "文字（Enter 換行）").font(theme::font(12.0)));
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
                ui.label(theme::muted(ui, "倍率").font(theme::font(12.0)));
                let mut z = cur.size / 100.0;
                ui.spacing_mut().slider_width = ui.available_width() - 60.0;
                if ui.add(egui::Slider::new(&mut z, 1.5..=4.0).step_by(0.25).fixed_decimals(2).suffix("×")).changed() {
                    cur.size = (z * 100.0).round();
                }
            });
        } else if cur.kind == AnnKind::Image {
            ui.horizontal(|ui| {
                ui.label(theme::muted(ui, "不透明度").font(theme::font(12.0)));
                let mut v = cur.size.clamp(5.0, 100.0);
                ui.spacing_mut().slider_width = ui.available_width() - 60.0;
                if ui.add(egui::Slider::new(&mut v, 5.0..=100.0).step_by(5.0).fixed_decimals(0).suffix("%")).changed() {
                    cur.size = v;
                }
            });
            ui.horizontal(|ui| {
                if Btn::new("換一張圖…").ghost().small().show(ui).clicked() {
                    ed.pending = Some(Act::PickPicture { replace: true });
                }
                if Btn::new("原始比例").ghost().small().tooltip("依圖片原本的長寬比例調整高度").show(ui).clicked() {
                    if let Some(pic) = cur.text.as_deref().and_then(picture::load) {
                        let cy = cur.y + cur.h / 2.0;
                        cur.h = (cur.w * pic.height() as f64 / pic.width().max(1) as f64).round().max(8.0);
                        cur.y = cy - cur.h / 2.0;
                    }
                }
            });
            if cur.text.as_deref().and_then(picture::load).is_none() {
                hint(ui, "找不到這張圖片（可能被移動或刪除了），請按「換一張圖」。");
            } else {
                hint(ui, "拖曳右下角調整大小（保持比例）；調低不透明度可以當作浮水印。");
            }
        } else if cur.kind.shape_only() {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.label(theme::muted(ui, "形狀").font(theme::font(12.0)));
                let mut shape = cur.shape.unwrap_or(Shape::Round);
                let items: Vec<(Shape, &str)> = Shape::ALL.iter().map(|s| (*s, s.label())).collect();
                if segmented(ui, &mut shape, &items, true) {
                    cur.shape = Some(shape);
                }
            });
            hint(ui, "框以外的地方會變暗，凸顯框裡的重點。");
        } else if cur.kind.is_effect() {
            let idx = (cur.kind == AnnKind::Blur) as usize;
            let word = if cur.kind == AnnKind::Mosaic { "馬賽克" } else { "模糊" };
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.label(theme::muted(ui, "形狀").font(theme::font(12.0)));
                let mut shape = cur.shape.unwrap_or_default();
                let items: Vec<(Shape, &str)> = Shape::ALL.iter().map(|s| (*s, s.label())).collect();
                if segmented(ui, &mut shape, &items, true) {
                    cur.shape = Some(shape);
                    ed.last_style[idx].0 = shape;
                }
            });
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.label(theme::muted(ui, "範圍").font(theme::font(12.0)));
                let mut inv = cur.invert;
                let (l0, l1) = (format!("框內{word}"), format!("框外{word}（框內清楚）"));
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
                switch(ui, &mut cur.bg, "深色底", true);
            }
        }

        // 同類的標註一次改成一樣（例如所有編號都改成綠色、一樣大）
        let same = ed.anns.iter().filter(|o| o.kind == cur.kind).count();
        // 表情符號不算（顏色、大小是另外選的）
        let emoji = cur.kind == AnnKind::Text && cur.text.as_deref().is_some_and(super::is_emoji_text);
        if same > 1 && !emoji && Btn::new(format!("全部 {same} 個{}都改成這樣", cur.kind.label())).ghost().small().tooltip("顏色、大小（以及形狀、底色）套用到所有同類的標註").show(ui).clicked()
        {
            apply_all = true;
        }
        // 角度（箭頭不用：兩端本來就能指向任何方向）
        if cur.kind.rotatable() {
            ui.horizontal(|ui| {
                ui.label(theme::muted(ui, "角度").font(theme::font(12.0)));
                let mut deg = cur.rot;
                ui.spacing_mut().slider_width = ui.available_width() - 120.0;
                if ui
                    .add(egui::Slider::new(&mut deg, -180.0..=180.0).step_by(1.0).fixed_decimals(0).suffix("°"))
                    .on_hover_text("也可以拖曳影片上選取框旁的旋轉鈕，或按 [ / ] 每次轉 15 度（加 Shift 每次 1 度）")
                    .changed()
                {
                    cur.rot = deg;
                }
                if Btn::new("歸零").ghost().small().enabled(cur.rot != 0.0).show(ui).clicked() {
                    cur.rot = 0.0;
                }
            });
        }

        // 出現時間（截圖沒有）
        if ed.is_shot() {
            return;
        }
        ui.horizontal(|ui| {
            ui.label(theme::muted(ui, "出現").font(theme::font(12.0)));
            for (i, sep) in [(0usize, Some("到")), (1, None)] {
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
            if Btn::new("從目前時間開始").ghost().small().tooltip("從播放頭的時間開始").show(ui).clicked() {
                cur.start = now.min(cur.end - 0.1);
            }
            if Btn::new("到目前時間結束").ghost().small().tooltip("到播放頭的時間結束").show(ui).clicked() {
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
        let xr = ui.interact(x_rect, Id::new(("ed-ann-del", id)), Sense::click()).on_hover_text("刪除這個標註").on_hover_cursor(CursorIcon::PointingHand);
        if xr.hovered() {
            ui.painter().circle_filled(x_rect.center(), 11.0, p.rec_soft);
        }
        ui.painter().text(x_rect.center(), Align2::CENTER_CENTER, "×", theme::font(14.0), if xr.hovered() { p.rec } else { p.text });
        if xr.clicked() {
            delete = Some(id);
        } else if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(if gone { "在刪除的片段中，不會出現在輸出影片" } else { "" }).clicked() {
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
