//! 在檔案庫的 Jellyfin 分頁直接改影片資訊：家人用手機傳上去的影片不用先下載，就能改標題、說明、人物、標籤、年份、封面。
//! 見 core::jellyfin 的 item_info、set_info、set_cover

use super::theme::{self, Btn, Icon};
use super::UiApp;
use eframe::egui::{self, Align, Id, Layout, RichText};
use screenrecorder_core::jellyfin::JfInfo;
use screenrecorder_core::{tr, trf};

pub struct JfEdit {
    id: String,
    user: String,
    name: String,
    /// 讀到的（判斷有沒有改過）；還在讀時是 None
    orig: Option<JfInfo>,
    info: JfInfo,
    year: String,
    /// 新增人物、標籤的輸入框
    new_person: String,
    new_tag: String,
    /// 伺服器上已經有的人物、標籤（點一下加入）
    known_people: Vec<String>,
    known_tags: Vec<String>,
    /// 選好的封面圖片（檔名, 圖片內容）
    cover: Option<(String, Vec<u8>)>,
    saving: bool,
    error: Option<String>,
}

/// 打開（背景讀這支影片目前的資訊）
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
        cover: None,
        saving: false,
        error: None,
    });
    let key = id.clone();
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
}

/// 一排可以拿掉的小標籤，加上輸入框（Enter 或「加入」）與伺服器上已經有的（點一下加入）
fn chips(ui: &mut egui::Ui, list: &mut Vec<String>, input: &mut String, known: &[String], hint: &str) {
    let mut remove = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        for (i, v) in list.iter().enumerate() {
            if theme::chip(ui, &format!("{v}  ×"), theme::Tone::Accent, true).on_hover_text(tr!("點一下拿掉", "Click to remove")).clicked() {
                remove = Some(i);
            }
        }
    });
    if let Some(i) = remove {
        list.remove(i);
    }
    ui.horizontal(|ui| {
        let r = ui.add(egui::TextEdit::singleline(input).desired_width(ui.available_width() - 70.0).hint_text(hint));
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (Btn::new(tr!("加入", "Add")).small().enabled(!input.trim().is_empty()).show(ui).clicked() || enter) && !input.trim().is_empty() {
            let v = input.trim().to_string();
            if !list.iter().any(|x| x.to_lowercase() == v.to_lowercase()) {
                list.push(v);
            }
            input.clear();
            if enter {
                r.request_focus();
            }
        }
    });
    let rest: Vec<&String> = known.iter().filter(|k| !list.iter().any(|x| x.to_lowercase() == k.to_lowercase())).take(16).collect();
    if !rest.is_empty() {
        let mut add = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            for k in rest {
                if theme::chip(ui, k, theme::Tone::Plain, true).on_hover_text(tr!("點一下加入", "Click to add")).clicked() {
                    add = Some(k.clone());
                }
            }
        });
        list.extend(add);
    }
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut d) = app.jf_edit.take() else { return };
    let mut close = false;
    let mut save = false;
    let mut pick_cover = false;
    let year = d.year.trim().parse::<i32>().ok().filter(|y| (1900..=2999).contains(y));
    let year_ok = d.year.trim().is_empty() || year.is_some();
    let changed = d.orig.as_ref().is_some_and(|o| *o != JfInfo { year, ..d.info.clone() }) || d.cover.is_some();
    let modal = egui::Modal::new(Id::new("jellyfin-edit")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        ui.set_width((ctx.content_rect().width() - 80.0).clamp(320.0, 540.0));
        let pal = theme::pal(ui);
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr!("Jellyfin 上的影片資訊", "Details on Jellyfin")).font(theme::font_bold(17.0)));
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if Btn::icon_only(Icon::Close).ghost().small().tooltip(tr!("關閉（Esc）", "Close (Esc)")).show(ui).clicked() {
                    close = true;
                }
            });
        });
        ui.add(egui::Label::new(RichText::new(&d.name).color(pal.muted)).truncate());
        ui.add_space(8.0);
        if let Some(e) = &d.error {
            ui.label(RichText::new(e).color(pal.warn).font(theme::font(12.5)));
        }
        if d.orig.is_none() {
            if d.error.is_none() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tr!("正在讀取…", "Reading…"));
                });
            }
        } else {
            ui.add_enabled_ui(!d.saving, |ui| {
                egui::Grid::new("jf-edit-form").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                    ui.label(tr!("標題", "Title"));
                    ui.add(egui::TextEdit::singleline(&mut d.info.title).desired_width(ui.available_width()));
                    ui.end_row();
                    ui.label(tr!("說明", "Description"));
                    ui.add(egui::TextEdit::multiline(&mut d.info.overview).desired_rows(3).desired_width(ui.available_width()));
                    ui.end_row();
                    ui.label(tr!("年份", "Year"));
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut d.year).desired_width(80.0).hint_text("2024"));
                        if !year_ok {
                            ui.label(RichText::new(tr!("要是 4 位數的年份", "Use a 4-digit year")).color(pal.warn).font(theme::font(12.0)));
                        }
                    });
                    ui.end_row();
                    ui.label(tr!("人物", "People"));
                    ui.vertical(|ui| chips(ui, &mut d.info.people, &mut d.new_person, &d.known_people, tr!("輸入名字，按 Enter 加入", "Type a name and press Enter")));
                    ui.end_row();
                    ui.label(tr!("標籤", "Tags"));
                    ui.vertical(|ui| chips(ui, &mut d.info.tags, &mut d.new_tag, &d.known_tags, tr!("輸入標籤，按 Enter 加入", "Type a tag and press Enter")));
                    ui.end_row();
                    ui.label(tr!("封面", "Cover"));
                    ui.horizontal(|ui| {
                        if Btn::new(tr!("選一張圖片…", "Choose an image…")).small().show(ui).clicked() {
                            pick_cover = true;
                        }
                        let text = match &d.cover {
                            Some((name, _)) => name.clone(),
                            None => tr!("不換", "Unchanged").to_string(),
                        };
                        ui.label(RichText::new(text).color(pal.muted).font(theme::font(12.5)));
                    });
                    ui.end_row();
                });
            });
            ui.add_space(6.0);
            ui.label(
                RichText::new(tr!(
                    "直接改 Jellyfin 上的資料，電視、手機上看得到；NAS 上的影片檔不會動。檔案庫 Jellyfin 分頁上方的「人物」「標籤」「年份」可以找出同一個人、同一個標籤的影片。",
                    "This changes the details on Jellyfin, so TVs and phones see them; the file on the NAS isn't touched. Use People, Tag and Year at the top of the library's Jellyfin tab to find matching videos."
                ))
                .font(theme::font(12.0))
                .color(pal.muted),
            );
        }
        ui.add_space(10.0);
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
    if (close || modal.should_close()) && !d.saving && app.ask.is_none() {
        return;
    }
    if pick_cover {
        app.jf_edit = Some(d);
        return choose_cover(app);
    }
    if save {
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
                    if let Some(d) = app.jf_edit.as_mut() {
                        d.saving = false;
                        d.error = Some(trf!("沒有存到 Jellyfin：{e}", "Couldn't save to Jellyfin: {e}"));
                    }
                }
            },
        );
    }
    app.jf_edit = Some(d);
}

/// 用 Windows 的開啟檔案視窗選封面（PNG、JPG 直接用；其他格式轉成 JPG）
fn choose_cover(app: &mut UiApp) {
    app.spawn(
        async move {
            tokio::task::spawn_blocking(|| -> Result<Option<(String, Vec<u8>)>, String> {
                let Some(path) = screenrecorder_core::picture::pick_file()? else { return Ok(None) };
                let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let jpeg_or_png = bytes.starts_with(b"\x89PNG") || bytes.starts_with(&[0xFF, 0xD8]);
                if jpeg_or_png {
                    return Ok(Some((name, bytes)));
                }
                let pm = screenrecorder_core::picture::decode_pixmap(&bytes).ok_or_else(|| tr!("看不懂這張圖片", "Couldn't read this image").to_string())?;
                Ok(Some((name, screenrecorder_core::shot_edit::encode_jpeg(&pm, 90)?)))
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
