//! 在檔案庫的 Jellyfin 分頁直接改影片資訊：家人用手機傳上去的影片不用先下載，就能改標題、說明、人物、標籤、年份、封面。
//! 存好之後重新讀回來比對，Jellyfin 沒有改到的欄位直接講出來（不會默默沒存到）。
//! 見 core::jellyfin 的 item_info、set_info、set_cover、unsaved

use super::theme::{self, Btn, Icon};
use super::UiApp;
use eframe::egui::{self, vec2, Align, Id, Layout, Rect, RichText, Sense, Stroke};
use screenrecorder_core::jellyfin::{self, JfInfo};
use screenrecorder_core::{tr, trf};

/// 封面預覽的大小（16:9）
const COVER: egui::Vec2 = vec2(192.0, 108.0);

pub struct JfEdit {
    id: String,
    user: String,
    name: String,
    /// 讀到的（判斷有沒有改過）；還在讀時是 None
    orig: Option<JfInfo>,
    info: JfInfo,
    year: String,
    /// 人物、標籤輸入框裡還沒變成小標籤的字（儲存時一起存）
    new_person: String,
    new_tag: String,
    /// 伺服器上已經有的人物、標籤（點一下加入）
    known_people: Vec<String>,
    known_tags: Vec<String>,
    /// 目前的封面（預覽用）
    poster: Option<egui::TextureHandle>,
    /// 選好的新封面
    cover: Option<Picked>,
    saving: bool,
    error: Option<String>,
}

/// 打開（背景讀這支影片目前的資訊和封面）
pub fn open(app: &mut UiApp, id: String, user: String, name: String, known_people: Vec<String>, known_tags: Vec<String>) {
    let Some(jf) = app.jellyfin() else { return };
    app.jf_edit = Some(JfEdit {
        id: id.clone(),
        user: user.clone(),
        name,
        orig: None,
        info: JfInfo::default(),
        year: String::new(),
        new_person: String::new(),
        new_tag: String::new(),
        known_people,
        known_tags,
        poster: None,
        cover: None,
        saving: false,
        error: None,
    });
    let (key, jf2, id2) = (id.clone(), jf.clone(), id.clone());
    app.spawn(async move { tokio::task::spawn_blocking(move || jf.item_info(&user, &key)).await.unwrap_or_else(|e| Err(e.to_string())) }, move |app, r| {
        let Some(d) = app.jf_edit.as_mut().filter(|d| d.id == id) else { return };
        match r {
            Ok(info) => {
                d.year = info.year.map(|y| y.to_string()).unwrap_or_default();
                d.info = info.clone();
                d.orig = Some(info);
            }
            Err(e) => d.error = Some(trf!("讀不到 Jellyfin 上的影片資訊：{e}", "Couldn't read the details from Jellyfin: {e}")),
        }
    });
    let ctx = app.ctx.clone();
    app.spawn(
        async move {
            let key = id2.clone();
            let img = tokio::task::spawn_blocking(move || jf2.poster(&key).and_then(|b| image_of(&b))).await.ok().flatten();
            (id2, img.map(|i| ctx.load_texture("jf-edit-poster", i, egui::TextureOptions::LINEAR)))
        },
        |app, (id, tex)| {
            if let Some(d) = app.jf_edit.as_mut().filter(|d| d.id == id) {
                d.poster = tex;
            }
        },
    );
}

/// 圖片檔（JPG、PNG…）轉成可以畫的圖
fn image_of(bytes: &[u8]) -> Option<egui::ColorImage> {
    let pm = screenrecorder_core::picture::decode_pixmap(bytes)?;
    Some(egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data()))
}

/// 輸入框裡的字變成小標籤（可以一次打好幾個，用逗號或頓號分開）；已經有的不重複加
fn commit(list: &mut Vec<String>, input: &mut String) {
    for v in input.split([',', '，', '、', ';', '；']).map(str::trim).filter(|v| !v.is_empty()) {
        if !list.iter().any(|x| x.to_lowercase() == v.to_lowercase()) {
            list.push(v.to_string());
        }
    }
    input.clear();
}

/// 欄位上面的小標題
fn label(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).font(theme::font_bold(12.5)).color(theme::pal(ui).muted));
}

/// 一個像輸入框的方框：裡面是已經加的小標籤（點 × 拿掉）和打字的地方（Enter、逗號加入；空的時按退格拿掉最後一個）。
/// 下面一排是伺服器上已經有的，點一下加入
fn chips(ui: &mut egui::Ui, id: &str, list: &mut Vec<String>, input: &mut String, known: &[String], hint: &str) {
    let p = theme::pal(ui);
    let mut remove = None;
    let focus_id = Id::new(("jf-edit-chips", id));
    let focused = ui.memory(|m| m.has_focus(focus_id));
    let frame = egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, if focused { p.accent } else { p.border })).corner_radius(6.0).inner_margin(egui::Margin::symmetric(6, 5)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            for (i, v) in list.iter().enumerate() {
                if theme::chip(ui, &format!("{v}  ×"), theme::Tone::Accent, true).on_hover_text(tr!("點一下拿掉", "Click to remove")).clicked() {
                    remove = Some(i);
                }
            }
            let w = (ui.available_size_before_wrap().x - 12.0).max(120.0);
            let r = ui.add(egui::TextEdit::singleline(input).id(focus_id).frame(egui::Frame::NONE).desired_width(w).hint_text(if list.is_empty() { hint } else { "" }));
            let (enter, back) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Backspace)));
            if r.changed() && input.contains([',', '，', '、', ';', '；']) {
                commit(list, input);
            }
            if r.lost_focus() && enter && !input.trim().is_empty() {
                commit(list, input);
                r.request_focus();
            }
            if r.has_focus() && back && input.is_empty() && !list.is_empty() && !r.changed() {
                remove = Some(list.len() - 1);
            }
        });
    });
    // 點方框的空白處也是開始打字
    if ui.interact(frame.response.rect, Id::new(("jf-edit-chips-box", id)), Sense::click()).clicked() {
        ui.memory_mut(|m| m.request_focus(focus_id));
    }
    if let Some(i) = remove {
        list.remove(i);
    }
    let rest: Vec<&String> = known.iter().filter(|k| !list.iter().any(|x| x.to_lowercase() == k.to_lowercase())).take(12).collect();
    if !rest.is_empty() {
        let mut add = None;
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            ui.label(RichText::new(tr!("常用：", "Used before:")).font(theme::font(12.0)).color(p.muted));
            for k in rest {
                if theme::chip(ui, &format!("＋ {k}"), theme::Tone::Plain, true).on_hover_text(tr!("點一下加入", "Click to add")).clicked() {
                    add = Some(k.clone());
                }
            }
        });
        list.extend(add);
    }
}

/// 封面預覽：選了新的就顯示新的，不然是 Jellyfin 上目前的
fn cover_box(ui: &mut egui::Ui, d: &JfEdit, pick: &mut bool, undo: &mut bool) {
    let p = theme::pal(ui);
    ui.vertical(|ui| {
        ui.set_width(COVER.x);
        label(ui, tr!("封面", "Cover"));
        let (rect, resp) = ui.allocate_exact_size(COVER, Sense::click());
        let tex = d.cover.as_ref().and_then(|c| c.2.as_ref()).or(d.poster.as_ref());
        ui.painter().rect_filled(rect, 8.0, p.surface2);
        match tex {
            Some(t) => theme::paint_thumb_cover(ui.painter(), rect, t),
            None => theme::paint_icon(ui.painter(), Rect::from_center_size(rect.center(), vec2(28.0, 28.0)), Icon::Video, p.muted),
        }
        if resp.hovered() {
            ui.painter().rect_filled(rect, 8.0, egui::Color32::from_black_alpha(110));
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, tr!("換一張…", "Change…"), theme::font_bold(13.0), egui::Color32::WHITE);
        }
        if d.cover.is_some() {
            ui.painter().rect_stroke(rect, 8.0, Stroke::new(2.0, p.accent), egui::StrokeKind::Inside);
        }
        if resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tr!("選一張圖片當封面", "Choose an image for the cover")).clicked() {
            *pick = true;
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| match &d.cover {
            Some((name, ..)) => {
                ui.add(egui::Label::new(RichText::new(trf!("新的：{name}", "New: {name}")).font(theme::font(12.0)).color(p.accent)).truncate());
                if Btn::new(tr!("不換", "Keep old")).small().ghost().show(ui).clicked() {
                    *undo = true;
                }
            }
            None => {
                if Btn::new(tr!("換封面…", "Change cover…")).small().show(ui).clicked() {
                    *pick = true;
                }
            }
        });
    });
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut d) = app.jf_edit.take() else { return };
    let mut close = false;
    let mut save = false;
    let mut pick_cover = false;
    let mut undo_cover = false;
    let year = d.year.trim().parse::<i32>().ok().filter(|y| (1900..=2999).contains(y));
    let year_ok = d.year.trim().is_empty() || year.is_some();
    // 輸入框裡還沒按 Enter 的字也算（儲存時會一起存）
    let pending = !d.new_person.trim().is_empty() || !d.new_tag.trim().is_empty();
    let changed = d.orig.as_ref().is_some_and(|o| *o != JfInfo { year, ..d.info.clone() }) || d.cover.is_some() || pending;
    let modal = egui::Modal::new(Id::new("jellyfin-edit")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        ui.set_width((ctx.content_rect().width() - 80.0).clamp(360.0, 600.0));
        let p = theme::pal(ui);
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr!("Jellyfin 上的影片資訊", "Details on Jellyfin")).font(theme::font_bold(17.0)));
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if Btn::icon_only(Icon::Close).ghost().small().tooltip(tr!("關閉（Esc）", "Close (Esc)")).show(ui).clicked() {
                    close = true;
                }
            });
        });
        ui.add(egui::Label::new(RichText::new(&d.name).color(p.muted)).truncate());
        ui.add_space(10.0);
        if d.orig.is_none() {
            match &d.error {
                Some(e) => {
                    ui.label(RichText::new(e).color(p.warn).font(theme::font(12.5)));
                }
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(tr!("正在讀取…", "Reading…"));
                    });
                }
            }
        } else {
            ui.add_enabled_ui(!d.saving, |ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                // 上面：左邊封面，右邊標題、年份
                ui.horizontal_top(|ui| {
                    cover_box(ui, &d, &mut pick_cover, &mut undo_cover);
                    ui.add_space(14.0);
                    ui.vertical(|ui| {
                        label(ui, tr!("標題", "Title"));
                        ui.add(egui::TextEdit::singleline(&mut d.info.title).desired_width(ui.available_width()).min_size(vec2(0.0, 30.0)).vertical_align(Align::Center));
                        ui.add_space(8.0);
                        label(ui, tr!("年份", "Year"));
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut d.year).desired_width(90.0).min_size(vec2(0.0, 30.0)).vertical_align(Align::Center).hint_text(tr!("例如 2024", "e.g. 2024")));
                            if !year_ok {
                                ui.label(RichText::new(tr!("要是 4 位數的年份", "Use a 4-digit year")).color(p.warn).font(theme::font(12.0)));
                            }
                        });
                    });
                });
                ui.add_space(10.0);
                label(ui, tr!("說明", "Description"));
                ui.add(
                    egui::TextEdit::multiline(&mut d.info.overview)
                        .desired_rows(3)
                        .desired_width(ui.available_width())
                        .hint_text(tr!("這支影片在拍什麼（電視上會顯示）", "What's in this video (shown on the TV)")),
                );
                ui.add_space(10.0);
                label(ui, tr!("人物", "People"));
                chips(
                    ui,
                    "people",
                    &mut d.info.people,
                    &mut d.new_person,
                    &d.known_people,
                    tr!("打名字，按 Enter 加入（可以一次打好幾個，用頓號分開）", "Type a name and press Enter (separate several with commas)"),
                );
                ui.add_space(10.0);
                label(ui, tr!("標籤", "Tags"));
                chips(ui, "tags", &mut d.info.tags, &mut d.new_tag, &d.known_tags, tr!("打標籤，按 Enter 加入", "Type a tag and press Enter"));
            });
            if let Some(e) = &d.error {
                ui.add_space(8.0);
                egui::Frame::new().fill(p.warn.gamma_multiply(0.12)).corner_radius(6.0).inner_margin(8.0).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(e).color(p.warn).font(theme::font(12.5)));
                });
            }
        }
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(tr!("存到 Jellyfin，電視、手機上都看得到；NAS 上的影片檔不會動", "Saved on Jellyfin for TVs and phones; the file on the NAS isn't touched"))
                    .font(theme::font(12.0))
                    .color(p.muted),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = if d.saving { tr!("正在儲存…", "Saving…") } else { tr!("儲存", "Save") };
                if Btn::new(label).primary().enabled(changed && year_ok && !d.saving).show(ui).clicked() {
                    save = true;
                }
                if Btn::new(tr!("取消", "Cancel")).ghost().show(ui).clicked() {
                    close = true;
                }
            });
        });
    });
    if (close || modal.should_close()) && !d.saving && app.ask.is_none() {
        return;
    }
    if undo_cover {
        d.cover = None;
    }
    if pick_cover {
        app.jf_edit = Some(d);
        return choose_cover(app);
    }
    if save {
        // 輸入框裡還沒按 Enter 的人物、標籤也存
        commit(&mut d.info.people, &mut d.new_person);
        commit(&mut d.info.tags, &mut d.new_tag);
        d.saving = true;
        d.error = None;
        let info = JfInfo { year, ..d.info.clone() };
        let (id, user, cover) = (d.id.clone(), d.user.clone(), d.cover.as_ref().map(|c| c.1.clone()));
        let Some(jf) = app.jellyfin() else { return };
        let (id2, title) = (id.clone(), info.title.clone());
        app.spawn(
            async move {
                tokio::task::spawn_blocking(move || {
                    jf.set_info(&user, &id, &info)?;
                    if let Some(c) = cover {
                        jf.set_cover(&id, &c)?;
                    }
                    // 重新讀回來：Jellyfin 沒改到的欄位要講出來
                    let got = jf.item_info(&user, &id)?;
                    let lost = jellyfin::unsaved(&info, &got);
                    if !lost.is_empty() {
                        screenrecorder_core::info!("[Jellyfin] 影片資訊沒存到：{lost:?}；想存 {info:?}，讀回來 {got:?}");
                        return Err(trf!(
                            "Jellyfin 回答存好了，但重新讀取時這些沒有改到：{}。請到 Jellyfin 網頁版這支影片的「編輯中繼資料」看看這些欄位有沒有被鎖住（鎖頭圖示）",
                            "Jellyfin said it saved, but reading it back these didn't change: {}. In the Jellyfin web app, open “Edit metadata” for this video and check whether these fields are locked",
                            lost.join(tr!("、", ", "))
                        ));
                    }
                    Ok::<_, String>(())
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string()))
            },
            move |app, r| match r {
                Ok(()) => {
                    let name = app.jf_edit.take().map(|d| d.name).unwrap_or_default();
                    super::jellyfin_browse::edited(app, &id2, &title);
                    app.toast(trf!("已更新 Jellyfin 上的影片資訊：{}", "Updated the details on Jellyfin: {}", if title.trim().is_empty() { name } else { title }), false);
                }
                Err(e) => {
                    super::jellyfin_browse::edited(app, &id2, "");
                    if let Some(d) = app.jf_edit.as_mut() {
                        d.saving = false;
                        d.error = Some(trf!("沒有完全存到 Jellyfin：{e}", "Not everything was saved to Jellyfin: {e}"));
                    }
                }
            },
        );
    }
    app.jf_edit = Some(d);
}

/// 選好的封面：（檔名, 圖片內容, 預覽）
type Picked = (String, Vec<u8>, Option<egui::TextureHandle>);

/// 用 Windows 的開啟檔案視窗選封面（PNG、JPG 直接用；其他格式轉成 JPG）
fn choose_cover(app: &mut UiApp) {
    let ctx = app.ctx.clone();
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || -> Result<Option<Picked>, String> {
                let Some(path) = screenrecorder_core::picture::pick_file()? else { return Ok(None) };
                let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let pm = screenrecorder_core::picture::decode_pixmap(&bytes).ok_or_else(|| tr!("看不懂這張圖片", "Couldn't read this image").to_string())?;
                let preview = Some(ctx.load_texture("jf-edit-cover", egui::ColorImage::from_rgba_premultiplied([pm.width() as usize, pm.height() as usize], pm.data()), egui::TextureOptions::LINEAR));
                let jpeg_or_png = bytes.starts_with(b"\x89PNG") || bytes.starts_with(&[0xFF, 0xD8]);
                if jpeg_or_png {
                    return Ok(Some((name, bytes, preview)));
                }
                Ok(Some((name, screenrecorder_core::shot_edit::encode_jpeg(&pm, 90)?, preview)))
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        },
        |app, r| match r {
            Ok(Some(c)) => {
                if let Some(d) = app.jf_edit.as_mut() {
                    d.cover = Some(c);
                }
            }
            Ok(None) => {}
            Err(e) => app.toast(e, true),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_names_become_chips() {
        let mut list = vec!["媽媽".to_string()];
        let mut input = " 小明、阿嬤, 媽媽 ".to_string();
        commit(&mut list, &mut input);
        assert_eq!(list, vec!["媽媽", "小明", "阿嬤"]);
        assert!(input.is_empty());
    }
}
