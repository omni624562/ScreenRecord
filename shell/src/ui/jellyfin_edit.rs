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
    /// 伺服器上已經有的人物、標籤與用在幾支影片（點一下加入；次數讀好之前是 0）
    known_people: Vec<(String, usize)>,
    known_tags: Vec<(String, usize)>,
    /// 「全部 N 個 ▾」清單（人物、標籤各一個）
    pick_people: Picker,
    pick_tags: Picker,
    /// 目前的封面（預覽用）
    poster: Option<egui::TextureHandle>,
    /// 選好的新封面
    cover: Option<Picked>,
    /// 存檔時拿掉 Jellyfin 上的封面
    remove_cover: bool,
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
        known_people: known_people.into_iter().map(|n| (n, 0)).collect(),
        known_tags: known_tags.into_iter().map(|n| (n, 0)).collect(),
        pick_people: Picker::default(),
        pick_tags: Picker::default(),
        poster: None,
        cover: None,
        remove_cover: false,
        saving: false,
        error: None,
    });
    // 全部影片用到的人物、標籤與次數（常用依次數排）；讀不到就維持原本的清單
    let (jf3, user3, id3) = (jf.clone(), user.clone(), id.clone());
    app.spawn(async move { tokio::task::spawn_blocking(move || jf3.usage(&user3)).await.unwrap_or_else(|e| Err(e.to_string())) }, move |app, r| {
        let Some(d) = app.jf_edit.as_mut().filter(|d| d.id == id3) else { return };
        match r {
            Ok((people, tags)) => {
                d.known_people = merge_usage(people, &d.known_people);
                d.known_tags = merge_usage(tags, &d.known_tags);
            }
            Err(e) => screenrecorder_core::info!("[Jellyfin] 讀不到人物、標籤的使用次數：{e}"),
        }
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

/// 使用次數（多的在前）加上伺服器上有、但還沒有影片用到的（次數 0）
fn merge_usage(mut used: Vec<(String, usize)>, known: &[(String, usize)]) -> Vec<(String, usize)> {
    for (n, _) in known {
        if !used.iter().any(|(u, _)| jellyfin::name_key(u) == jellyfin::name_key(n)) {
            used.push((n.clone(), 0));
        }
    }
    used
}

/// 「全部 N 個 ▾」清單的狀態：搜尋的字、排序、勾了哪些
#[derive(Default)]
struct Picker {
    search: String,
    sort: Sort,
    checked: Vec<String>,
}

#[derive(Default, Clone, Copy, PartialEq)]
enum Sort {
    #[default]
    Count,
    Name,
    Recent,
}

/// 最近加過的放到最前面（最多 RECENT_GENRES 個）
fn remember(recent: &mut Vec<String>, names: &[String]) {
    for n in names.iter().rev() {
        recent.retain(|r| jellyfin::name_key(r) != jellyfin::name_key(n));
        recent.insert(0, n.clone());
    }
    recent.truncate(super::settings::RECENT_GENRES);
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
fn chips(ui: &mut egui::Ui, id: &str, list: &mut Vec<String>, input: &mut String, known: &[(String, usize)], prefs: Prefs, hint: &str) {
    let p = theme::pal(ui);
    let mut remove = None;
    // 點了名字（不是 ×）：拿回輸入框改
    let mut edit = None;
    let focus_id = Id::new(("jf-edit-chips", id));
    let focused = ui.memory(|m| m.has_focus(focus_id));
    egui::Frame::new().fill(p.surface).stroke(Stroke::new(1.0, if focused { p.accent } else { p.border })).corner_radius(6.0).inner_margin(egui::Margin::symmetric(6, 5)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
            for (i, v) in list.iter().enumerate() {
                let r = theme::chip(ui, &format!("{v}  ×"), theme::Tone::Accent, true).on_hover_text(tr!("點名字修改，點 × 拿掉", "Click the name to edit it, × to remove it"));
                if r.clicked() {
                    // 右邊的 × 是拿掉，其他地方是修改
                    let on_x = r.interact_pointer_pos().is_some_and(|pos| pos.x > r.rect.right() - 20.0);
                    if on_x {
                        remove = Some(i);
                    } else {
                        edit = Some(i);
                    }
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
    if let Some(i) = remove {
        list.remove(i);
    }
    // 修改：拿回輸入框（輸入框裡原本打的字先加進去），改好按 Enter
    if let Some(i) = edit {
        commit(list, input);
        let i = i.min(list.len().saturating_sub(1));
        if i < list.len() {
            *input = list.remove(i);
            ui.memory_mut(|m| m.request_focus(focus_id));
        }
    }
    suggestions(ui, id, list, input, known, prefs);
}

/// 這台電腦記住的：釘選的（可以改）、最近加過的
struct Prefs<'a> {
    pins: &'a mut Vec<String>,
    recent: &'a [String],
    /// 改了釘選（要存設定）
    changed: &'a mut bool,
    /// 「全部 N 個 ▾」清單的狀態
    picker: &'a mut Picker,
}

/// 輸入框下面：沒打字時一行「常用」（釘選的在前，再依使用次數；放得下幾個就幾個），最後是「全部 N 個 ▾」清單；
/// 打字時列出全部符合的
fn suggestions(ui: &mut egui::Ui, id: &str, list: &mut Vec<String>, input: &mut String, known: &[(String, usize)], prefs: Prefs) {
    let p = theme::pal(ui);
    let key = jellyfin::name_key;
    // 候選：伺服器上有的＋釘選的（伺服器上已經沒有的也列），已經加的不列
    let mut cands: Vec<(String, usize)> = known.to_vec();
    for pin in prefs.pins.iter() {
        if !cands.iter().any(|(n, _)| key(n) == key(pin)) {
            cands.push((pin.clone(), 0));
        }
    }
    cands.retain(|(n, _)| !list.iter().any(|x| key(x) == key(n)));
    let pins_now = prefs.pins.clone();
    let pin_rank = |n: &str| pins_now.iter().position(|p| key(p) == key(n));
    // 釘選的在前（照釘選的順序），其他依次數（known 已經是次數多的在前）
    let mut row: Vec<&(String, usize)> = cands.iter().collect();
    row.sort_by_key(|(n, _)| pin_rank(n).unwrap_or(usize::MAX));
    let typed = input.trim().to_lowercase();
    let mut add: Vec<String> = vec![];
    if !typed.is_empty() {
        let matches: Vec<&&(String, usize)> = row.iter().filter(|(n, _)| n.to_lowercase().contains(&typed) || key(n).contains(&key(&typed))).collect();
        if matches.is_empty() {
            if !known.is_empty() {
                ui.label(RichText::new(tr!("伺服器上沒有符合的，按 Enter 新增", "No match on the server; press Enter to add it")).font(theme::font(12.0)).color(p.muted));
            }
            return;
        }
        ui.add_space(2.0);
        egui::ScrollArea::vertical().id_salt(("jf-edit-known", id)).max_height(150.0).auto_shrink([false, true]).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                ui.label(RichText::new(tr!("符合的：", "Matches:")).font(theme::font(12.0)).color(p.muted));
                for (n, c) in matches {
                    if sug_chip(ui, n, *c, pin_rank(n).is_some()).clicked() {
                        add.push(n.clone());
                    }
                }
            });
        });
    } else if !cands.is_empty() {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(RichText::new(tr!("常用：", "Used before:")).font(theme::font(12.0)).color(p.muted));
            let all = Btn::new(trf!("全部 {} 個", "All {}", cands.len())).small().menu().tooltip(tr!("搜尋、勾選多個一次加入、釘選常用的", "Search, add several at once, pin favorites"));
            let all_w = all.width(ui) + 6.0;
            // 一行放得下的才放，「全部」那一顆一定留位置
            for (n, c) in &row {
                if sug_width(ui, n, *c, pin_rank(n).is_some()) + 6.0 > ui.available_width() - all_w {
                    break;
                }
                if sug_chip(ui, n, *c, pin_rank(n).is_some()).clicked() {
                    add.push(n.clone());
                }
            }
            let resp = all.show(ui);
            egui::Popup::menu(&resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                if let Some(picked) = picker_body(ui, id, &cands, prefs.pins, prefs.recent, prefs.changed, prefs.picker) {
                    add.extend(picked);
                    ui.close();
                }
            });
        });
    }
    if !add.is_empty() {
        for n in add {
            if !list.iter().any(|x| key(x) == key(&n)) {
                list.push(n);
            }
        }
        // 打了一半的字是用來找的：加好就清掉
        input.clear();
    }
}

/// 常用的一顆：「＋ 名稱」（釘選的「★ 名稱」），後面小小的灰字是用在幾支影片
fn sug_width(ui: &egui::Ui, name: &str, count: usize, pinned: bool) -> f32 {
    let p = theme::pal(ui);
    let t = ui.painter().layout_no_wrap(if pinned { format!("  {name}") } else { format!("＋ {name}") }, theme::font(12.5), p.text).size().x + if pinned { 8.0 } else { 0.0 };
    let c = if count > 0 { ui.painter().layout_no_wrap(count.to_string(), theme::font(11.0), p.muted).size().x + 5.0 } else { 0.0 };
    t + c + 20.0
}

fn sug_chip(ui: &mut egui::Ui, name: &str, count: usize, pinned: bool) -> egui::Response {
    let p = theme::pal(ui);
    let w = sug_width(ui, name, count, pinned);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 24.0), Sense::click());
    let fill = if pinned { p.warn_soft } else { p.surface2 };
    ui.painter().rect_filled(rect, 12.0, if resp.hovered() { fill.gamma_multiply(1.15) } else { fill });
    // 釘選的前面是星星（畫的圖示），其他是「＋」
    let mut x = rect.left() + 10.0;
    let t = if pinned {
        theme::paint_icon(ui.painter(), Rect::from_center_size(egui::pos2(x + 6.0, rect.center().y), vec2(12.0, 12.0)), Icon::Star, p.warn);
        x += 8.0;
        ui.painter().layout_no_wrap(format!("  {name}"), theme::font(12.5), p.text)
    } else {
        ui.painter().layout_no_wrap(format!("＋ {name}"), theme::font(12.5), p.text)
    };
    let tw = t.size().x + x - rect.left() - 10.0;
    ui.painter().galley(egui::pos2(x, rect.center().y - t.size().y / 2.0), t, p.text);
    if count > 0 {
        let c = ui.painter().layout_no_wrap(count.to_string(), theme::font(11.0), p.muted);
        ui.painter().galley(egui::pos2(rect.left() + 15.0 + tw, rect.center().y - c.size().y / 2.0), c, p.muted);
    }
    let tip = if count > 0 { trf!("點一下加入（用在 {count} 支影片）", "Click to add (used in {count} videos)") } else { tr!("點一下加入", "Click to add").to_string() };
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip)
}

/// 「全部 N 個 ▾」清單：搜尋、排序（次數 / 名稱 / 最近用過）、☆ 釘選、勾選多個一次加入；回傳要加入的
fn picker_body(ui: &mut egui::Ui, id: &str, cands: &[(String, usize)], pins: &mut Vec<String>, recent: &[String], changed: &mut bool, pk: &mut Picker) -> Option<Vec<String>> {
    let p = theme::pal(ui);
    let key = jellyfin::name_key;
    // 寬度固定：清單在選單裡，靠右的東西不能讓它每一格越撐越寬
    const W: f32 = 330.0;
    ui.set_width(W);
    ui.set_max_width(W);
    let hint = if id == "people" { tr!("搜尋人物…", "Search people…") } else { tr!("搜尋標籤…", "Search tags…") };
    ui.add(egui::TextEdit::singleline(&mut pk.search).hint_text(hint).desired_width(W));
    ui.add_space(4.0);
    theme::segmented(ui, &mut pk.sort, &[(Sort::Count, tr!("依次數", "By count")), (Sort::Name, tr!("依名稱", "By name")), (Sort::Recent, tr!("最近用過", "Recent"))], true);
    ui.add_space(4.0);
    let q = pk.search.trim().to_lowercase();
    let mut items: Vec<&(String, usize)> = cands.iter().filter(|(n, _)| q.is_empty() || n.to_lowercase().contains(&q) || key(n).contains(&key(&q))).collect();
    match pk.sort {
        Sort::Count => {}
        Sort::Name => items.sort_by_key(|(n, _)| n.to_lowercase()),
        Sort::Recent => items.sort_by_key(|(n, _)| recent.iter().position(|r| key(r) == key(n)).unwrap_or(usize::MAX)),
    }
    let (pinned, others): (Vec<_>, Vec<_>) = items.into_iter().partition(|(n, _)| pins.iter().any(|x| key(x) == key(n)));
    let mut toggle_pin: Option<String> = None;
    egui::ScrollArea::vertical().id_salt(("jf-edit-all", id)).max_height(300.0).show(ui, |ui| {
        if pinned.is_empty() && others.is_empty() {
            ui.label(RichText::new(tr!("沒有符合的", "No matches")).font(theme::font(12.5)).color(p.muted));
        }
        for (title, group) in [(tr!("釘選", "Pinned"), &pinned), (tr!("全部", "All"), &others)] {
            if group.is_empty() {
                continue;
            }
            if !pinned.is_empty() {
                ui.label(RichText::new(title).font(theme::font(11.5)).color(p.muted));
            }
            for (n, c) in group.iter() {
                let is_pin = !std::ptr::eq(group, &others);
                // 每一列固定大小：星星、勾選框＋名稱、右邊的次數（畫上去，不用靠右的排版，選單才不會越撐越寬）
                let (row, _) = ui.allocate_exact_size(vec2(W - 16.0, 26.0), Sense::hover());
                let tip = if is_pin { tr!("取消釘選", "Unpin") } else { tr!("釘選：永遠排在常用的最前面", "Pin: always first in the list") };
                let sr = Rect::from_min_size(row.min + vec2(0.0, 2.0), vec2(22.0, 22.0));
                let star = ui.interact(sr, Id::new(("jf-pin", id, n.as_str())), Sense::click());
                if star.hovered() {
                    ui.painter().rect_filled(sr, 5.0, p.surface2);
                }
                theme::paint_icon(ui.painter(), sr.shrink(4.0), Icon::Star, if is_pin { p.warn } else { p.muted.gamma_multiply(0.6) });
                if star.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip).clicked() {
                    toggle_pin = Some(n.clone());
                }
                let cw = if *c > 0 { 56.0 } else { 0.0 };
                let mut on = pk.checked.iter().any(|x| key(x) == key(n));
                let cb = Rect::from_min_max(egui::pos2(sr.max.x + 6.0, row.min.y), egui::pos2(row.max.x - cw, row.max.y));
                let clicked = ui.scope_builder(egui::UiBuilder::new().max_rect(cb).layout(Layout::left_to_right(Align::Center)), |ui| theme::check(ui, &mut on, n.as_str()).changed()).inner;
                if clicked {
                    if on {
                        pk.checked.push(n.clone());
                    } else {
                        pk.checked.retain(|x| key(x) != key(n));
                    }
                }
                if *c > 0 {
                    ui.painter().text(egui::pos2(row.max.x - 4.0, row.center().y), egui::Align2::RIGHT_CENTER, trf!("{c} 支", "{c}"), theme::font(12.0), p.muted);
                }
            }
        }
    });
    if let Some(n) = toggle_pin {
        if pins.iter().any(|x| key(x) == key(&n)) {
            pins.retain(|x| key(x) != key(&n));
        } else {
            pins.push(n);
        }
        *changed = true;
    }
    ui.separator();
    let mut out = None;
    let n = pk.checked.len();
    let (row, _) = ui.allocate_exact_size(vec2(W, 30.0), Sense::hover());
    ui.painter().text(egui::pos2(row.min.x, row.center().y), egui::Align2::LEFT_CENTER, trf!("已勾 {n} 個", "{n} checked"), theme::font(12.0), p.muted);
    let add = Btn::new(trf!("加入 {n} 個", "Add {n}")).primary().small().enabled(n > 0);
    let bw = add.width(ui);
    let br = Rect::from_min_max(egui::pos2(row.max.x - bw, row.min.y), row.max);
    if ui.scope_builder(egui::UiBuilder::new().max_rect(br), |ui| add.show(ui)).inner.clicked() {
        out = Some(std::mem::take(&mut pk.checked));
        pk.search.clear();
    }
    out
}

/// 封面預覽：選了新的就顯示新的，不然是 Jellyfin 上目前的
fn cover_box(ui: &mut egui::Ui, d: &mut JfEdit, pick: &mut bool, undo: &mut bool) {
    let p = theme::pal(ui);
    ui.vertical(|ui| {
        ui.set_width(COVER.x);
        label(ui, tr!("封面", "Cover"));
        let (rect, resp) = ui.allocate_exact_size(COVER, Sense::click());
        let tex = d.cover.as_ref().and_then(|c| c.2.as_ref()).or(d.poster.as_ref().filter(|_| !d.remove_cover));
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
            None if d.remove_cover => {
                ui.label(RichText::new(tr!("存檔時拿掉", "Removed on save")).font(theme::font(12.0)).color(p.warn));
                if Btn::new(tr!("不拿掉", "Keep it")).small().ghost().show(ui).clicked() {
                    d.remove_cover = false;
                }
            }
            None => {
                if Btn::new(tr!("換封面…", "Change cover…")).small().show(ui).clicked() {
                    *pick = true;
                }
                if d.poster.is_some()
                    && Btn::new(tr!("拿掉", "Remove"))
                        .small()
                        .ghost()
                        .tooltip(tr!("拿掉 Jellyfin 上的封面（Jellyfin 之後可能自己從影片取一張）", "Remove the cover on Jellyfin (Jellyfin may pick a frame from the video later)"))
                        .show(ui)
                        .clicked()
                {
                    d.remove_cover = true;
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
    let mut prefs_changed = false;
    let year = d.year.trim().parse::<i32>().ok().filter(|y| (1900..=2999).contains(y));
    let year_ok = d.year.trim().is_empty() || year.is_some();
    // 輸入框裡還沒按 Enter 的字也算（儲存時會一起存）
    let pending = !d.new_person.trim().is_empty() || !d.new_tag.trim().is_empty();
    let changed = d.orig.as_ref().is_some_and(|o| *o != JfInfo { year, ..d.info.clone() }) || d.cover.is_some() || d.remove_cover || pending;
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
                    cover_box(ui, &mut d, &mut pick_cover, &mut undo_cover);
                    ui.add_space(14.0);
                    ui.vertical(|ui| {
                        label(ui, tr!("標題", "Title"));
                        ui.add(egui::TextEdit::singleline(&mut d.info.title).desired_width(ui.available_width()).min_size(vec2(0.0, 30.0)).vertical_align(Align::Center));
                        if d.info.title.trim().is_empty() {
                            ui.label(RichText::new(tr!("Jellyfin 的標題不能空白：空白時保留原本的", "Jellyfin titles can't be empty; the current one is kept")).color(p.warn).font(theme::font(12.0)));
                        }
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
                    Prefs { pins: &mut app.s.jf_pin_people, recent: &app.s.jf_recent_people, changed: &mut prefs_changed, picker: &mut d.pick_people },
                    tr!("打名字，按 Enter 加入（可以一次打好幾個，用頓號分開）", "Type a name and press Enter (separate several with commas)"),
                );
                ui.add_space(10.0);
                label(ui, tr!("標籤", "Tags"));
                chips(
                    ui,
                    "tags",
                    &mut d.info.tags,
                    &mut d.new_tag,
                    &d.known_tags,
                    Prefs { pins: &mut app.s.jf_pin_tags, recent: &app.s.jf_recent_tags, changed: &mut prefs_changed, picker: &mut d.pick_tags },
                    tr!("打標籤，按 Enter 加入", "Type a tag and press Enter"),
                );
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
                RichText::new(tr!("存到 Jellyfin，電視、手機上都看得到；伺服器上的影片檔不會動", "Saved on Jellyfin for TVs and phones; the video file on the server isn't touched"))
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
    if prefs_changed {
        app.save_settings();
    }
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
        // 這次新加的記成「最近用過」
        if let Some(o) = &d.orig {
            let new = |now: &[String], before: &[String]| -> Vec<String> { now.iter().filter(|n| !before.iter().any(|b| jellyfin::name_key(b) == jellyfin::name_key(n))).cloned().collect() };
            let (np, nt) = (new(&d.info.people, &o.people), new(&d.info.tags, &o.tags));
            if !np.is_empty() || !nt.is_empty() {
                remember(&mut app.s.jf_recent_people, &np);
                remember(&mut app.s.jf_recent_tags, &nt);
                app.save_settings();
            }
        }
        d.saving = true;
        d.error = None;
        let info = JfInfo { year, ..d.info.clone() };
        let (id, user, cover, remove_cover) = (d.id.clone(), d.user.clone(), d.cover.as_ref().map(|c| c.1.clone()), d.remove_cover);
        let Some(jf) = app.jellyfin() else { return };
        let (id2, title) = (id.clone(), info.title.clone());
        app.spawn(
            async move {
                tokio::task::spawn_blocking(move || {
                    jf.set_info(&user, &id, &info)?;
                    if let Some(c) = cover {
                        jf.set_cover(&id, &c)?;
                    } else if remove_cover {
                        jf.remove_cover(&id)?;
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
                    d.remove_cover = false;
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
