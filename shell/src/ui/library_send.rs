//! 傳到媒體庫：選要放進媒體庫（例如 NAS 的 video）的哪個子資料夾（只有第一層）；
//! 以及清單上「已在媒體庫」的標註要用的索引（NAS 上的檔名與大小，背景讀、記起來）。

use super::theme::{self, Btn};
use super::{dialogs, UiApp};
use eframe::egui::{self, vec2, Align, Id, Layout, RichText};
use screenrecorder_core::jellyfin;
use screenrecorder_core::{tr, trf};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// 「傳到媒體庫」的選資料夾視窗
pub struct SendDlg {
    paths: Vec<String>,
    /// 媒體庫第一層的子資料夾；None = 還在讀（NAS 可能要一下子）
    subs: Option<Vec<String>>,
    /// 選的子資料夾（空的 = 最上層）
    chosen: String,
    /// 新增資料夾的名稱（Some = 正在輸入）
    new_name: Option<String>,
    /// 已經在媒體庫的哪些子資料夾（同名同大小；最上層是空字串）
    already: Vec<String>,
}

/// 媒體庫的索引：第一層的子資料夾，和小寫檔名 → (在哪個子資料夾, 大小)
pub struct LibIndex {
    root: String,
    at: Instant,
    subs: Vec<String>,
    map: HashMap<String, Vec<(String, u64)>>,
}

/// 索引多久重讀一次（別的電腦、Jellyfin 也可能放檔案進去）
const INDEX_TTL: Duration = Duration::from_secs(120);

/// 顯示用的子資料夾名稱：最上層顯示媒體庫資料夾的名稱
fn label(app: &UiApp, sub: &str) -> String {
    if sub.is_empty() {
        app.library_folder().unwrap_or_default()
    } else {
        sub.to_string()
    }
}

impl UiApp {
    /// 「傳到媒體庫」：先選要放進哪個子資料夾（預先選好上次用的；已經傳過的選它在的那個）
    pub fn send_to_library(&mut self, path: &str) {
        self.send_many_to_library(vec![path.to_string()]);
    }

    /// 勾選多支一起傳：只選一次資料夾
    pub fn send_many_to_library(&mut self, paths: Vec<String>) {
        if paths.is_empty() {
            return;
        }
        if self.library_folder().is_none() {
            self.toast(
                tr!("請先在「媒體庫」選資料夾（例如 Jellyfin 的媒體庫），之後就能直接傳", "Choose a library folder first (such as your Jellyfin library); then you can send files there"),
                false,
            );
            return super::settings_dialog::open(self, super::settings_dialog::Page::Save);
        }
        let root = self.s.after_copy_to.clone();
        let last = self.s.library_last_sub.clone();
        self.send_dlg = Some(SendDlg { paths: paths.clone(), subs: None, chosen: last, new_name: None, already: vec![] });
        self.spawn(
            async move {
                tokio::task::spawn_blocking(move || {
                    let subs = jellyfin::subfolders(&root);
                    let mut already: Vec<String> = vec![];
                    for p in &paths {
                        let file = std::path::Path::new(p);
                        let (Some(name), Ok(m)) = (file.file_name(), std::fs::metadata(file)) else { continue };
                        for found in jellyfin::find_in_library(&root, &name.to_string_lossy(), Some(m.len())) {
                            // 找到的只會在最上層或第一層子資料夾
                            let Some(dir) = found.parent() else { continue };
                            let sub = if same(dir, &root) { String::new() } else { dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() };
                            if !already.contains(&sub) {
                                already.push(sub);
                            }
                        }
                    }
                    (subs, already)
                })
                .await
                .unwrap_or_default()
            },
            |app, (subs, already)| {
                let Some(d) = app.send_dlg.as_mut() else { return };
                // 上次選的資料夾不在了：放最上層
                if !d.chosen.is_empty() && !subs.iter().any(|s| s.eq_ignore_ascii_case(&d.chosen)) {
                    d.chosen.clear();
                }
                // 只傳一支、而且已經在某個資料夾：預先選它在的那個（再傳一次就是更新那一份）
                if let [only] = already.as_slice() {
                    d.chosen = only.clone();
                }
                d.subs = Some(subs);
                d.already = already;
            },
        );
    }

    /// 真的傳過去（sub：媒體庫的哪個子資料夾，空的 = 最上層）
    pub fn send_now(&mut self, path: &str, sub: &str) {
        let Some(_) = self.library_folder() else { return };
        let jf = jellyfin::Jellyfin::new(&self.s.jellyfin_url, &self.s.jellyfin_key);
        let dest = jellyfin::library_dest(&self.s.after_copy_to, sub);
        let file = std::path::PathBuf::from(path);
        self.toast(trf!("正在傳到「{}」：{}", "Sending to “{}”: {}", label(self, sub), dialogs::file_name(path)), false);
        let ffmpeg = self.core.ffmpeg_path();
        self.spawn(
            async move {
                // 影片資訊的標籤、有沒有章節：傳好之後設到 Jellyfin
                let extras = jellyfin::Extras::read(ffmpeg.as_deref(), &file).await;
                tokio::task::spawn_blocking(move || jellyfin::send_file(jf.as_ref(), &dest, &file, &extras)).await.ok().flatten()
            },
            |app, r| {
                // 媒體庫的檔案變了：標註重讀
                app.lib_index = None;
                match r {
                    Some(Ok(msg)) => app.toast(msg, false),
                    Some(Err(e)) => app.toast(e, true),
                    None => {}
                }
            },
        );
    }

    /// 剪輯、加速版另存好時自動傳：放到「錄完自動傳」的那個子資料夾
    pub fn send_auto(&mut self, path: &str) {
        let sub = self.s.after_copy_sub.clone();
        self.send_now(path, &sub);
    }

    /// 媒體庫的索引（NAS 上的檔名、大小、子資料夾）；還沒讀過或太舊時在背景重讀，這次先回傳舊的或 None
    fn lib_index(&mut self) -> Option<&LibIndex> {
        let root = self.s.after_copy_to.trim().to_string();
        if root.is_empty() {
            return None;
        }
        let fresh = self.lib_index.as_ref().is_some_and(|i| i.root == root && i.at.elapsed() < INDEX_TTL);
        if !fresh && !self.lib_index_loading {
            self.lib_index_loading = true;
            let r = root.clone();
            self.spawn(async move { tokio::task::spawn_blocking(move || (jellyfin::subfolders(&r), jellyfin::library_index(&r), r)).await.ok() }, |app, got| {
                app.lib_index_loading = false;
                if let Some((subs, map, root)) = got {
                    app.lib_index = Some(LibIndex { root, at: Instant::now(), subs, map });
                    // 「還沒傳到媒體庫」的清單要用新的索引重查
                    if let Some(d) = app.library.as_mut() {
                        d.library_index_changed();
                    }
                }
            });
        }
        self.lib_index.as_ref().filter(|i| i.root == root)
    }

    /// 媒體庫裡有的檔案（小寫檔名, 大小）：「還沒傳到媒體庫」篩選用；還沒讀到時是空的
    pub fn library_keys(&mut self) -> Vec<(String, u64)> {
        self.lib_index().map(|i| i.map.iter().flat_map(|(name, at)| at.iter().map(move |(_, size)| (name.clone(), *size))).collect()).unwrap_or_default()
    }

    /// 媒體庫第一層的子資料夾（設定頁選「自動傳到」用）；還沒讀到時 None
    pub fn library_subs(&mut self) -> Option<Vec<String>> {
        self.lib_index().map(|i| i.subs.clone())
    }

    /// 這個檔案在媒體庫的哪裡（同名同大小）；回傳顯示用的資料夾名稱（最上層是媒體庫資料夾的名稱）。
    /// 本來就放在媒體庫裡的檔案、還沒讀到索引時回傳 None
    pub fn library_where(&mut self, path: &str, size: u64) -> Option<Vec<String>> {
        let root = self.s.after_copy_to.trim().to_string();
        if root.is_empty() || jellyfin::in_library(&root, std::path::Path::new(path)) {
            return None;
        }
        let folder = self.library_folder().unwrap_or_default();
        let name = std::path::Path::new(path).file_name()?.to_string_lossy().to_lowercase();
        let idx = self.lib_index()?;
        let hits: Vec<String> = idx.map.get(&name)?.iter().filter(|(_, s)| *s == size).map(|(sub, _)| if sub.is_empty() { folder.clone() } else { sub.clone() }).collect();
        (!hits.is_empty()).then_some(hits)
    }
}

/// 兩個資料夾是不是同一個（不分大小寫、/ 和 \ 都算）
fn same(a: &std::path::Path, b: &str) -> bool {
    let norm = |s: String| s.replace('\\', "/").trim_end_matches('/').to_lowercase();
    norm(a.display().to_string()) == norm(b.to_string())
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut d) = app.send_dlg.take() else { return };
    let folder = app.library_folder().unwrap_or_default();
    let mut close = false;
    let mut send = false;
    let modal = egui::Modal::new(Id::new("send-library")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        ui.set_width(460.0);
        let p = theme::pal(ui);
        let title = if d.paths.len() == 1 { tr!("傳到媒體庫", "Send to library").to_string() } else { trf!("傳 {} 支到媒體庫", "Send {} files to the library", d.paths.len()) };
        ui.label(RichText::new(title).font(theme::font_bold(16.0)));
        ui.add_space(4.0);
        ui.label(RichText::new(trf!("要放進「{folder}」的哪個資料夾？", "Which folder in “{folder}”?")).color(p.muted));
        ui.add_space(8.0);
        match &d.subs {
            None => {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(14.0));
                    ui.label(theme::muted(ui, tr!("正在讀取媒體庫的資料夾…", "Reading the library folders…")));
                });
            }
            Some(subs) => {
                let subs = subs.clone();
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                    let top = trf!("{folder}（最上層）", "{folder} (top level)");
                    if Btn::new(top).small().selected(d.chosen.is_empty()).show(ui).clicked() {
                        d.chosen.clear();
                    }
                    for s in &subs {
                        if Btn::new(s).small().selected(d.chosen.eq_ignore_ascii_case(s)).show(ui).clicked() {
                            d.chosen = s.clone();
                            d.new_name = None;
                        }
                    }
                    if d.new_name.is_none() && Btn::new(tr!("＋ 新增資料夾", "+ New folder")).small().ghost().show(ui).clicked() {
                        d.new_name = Some(String::new());
                    }
                });
                if subs.is_empty() {
                    ui.add_space(4.0);
                    ui.label(theme::muted(
                        ui,
                        tr!("媒體庫裡還沒有子資料夾（或 NAS 連不到），會放在最上層", "The library has no subfolders yet (or the NAS can't be reached); files go to the top level"),
                    ));
                }
            }
        }
        if let Some(name) = &mut d.new_name {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(theme::muted(ui, tr!("新資料夾", "New folder")));
                let r = ui.add(egui::TextEdit::singleline(name).hint_text(tr!("例如：家庭", "e.g. Family")).desired_width(220.0));
                if r.changed() {
                    d.chosen = name.trim().to_string();
                }
            });
            if !jellyfin::valid_subfolder(name) {
                ui.label(RichText::new(tr!("名稱不能有 \\ / : * ? \" < > | 這些符號", "The name can't contain \\ / : * ? \" < > |")).color(p.rec).font(theme::font(12.0)));
            }
        }
        if !d.already.is_empty() {
            ui.add_space(8.0);
            let at = d.already.iter().map(|s| label(app, s)).collect::<Vec<_>>().join(tr!("、", ", "));
            let same_place = d.already.iter().any(|s| s.eq_ignore_ascii_case(&d.chosen));
            let text = if same_place {
                trf!("已經在「{at}」裡了，再傳一次會換成這台電腦的版本", "Already in “{at}”; sending again replaces it with the version on this PC")
            } else {
                trf!("已經在「{at}」裡了，再傳會在選的資料夾多一份", "Already in “{at}”; sending puts another copy in the chosen folder")
            };
            ui.label(RichText::new(text).color(p.warn).font(theme::font(12.5)));
        }
        ui.add_space(12.0);
        let valid = d.subs.is_some() && jellyfin::valid_subfolder(&d.chosen) && d.new_name.as_ref().is_none_or(|n| !n.trim().is_empty());
        ui.allocate_ui_with_layout(vec2(ui.available_width(), 34.0), Layout::right_to_left(Align::Center), |ui| {
            let to = if d.chosen.is_empty() { folder.clone() } else { d.chosen.clone() };
            if Btn::new(trf!("傳到「{to}」", "Send to “{to}”")).primary().enabled(valid).show(ui).clicked() {
                send = true;
            }
            if Btn::new(tr!("取消", "Cancel")).ghost().show(ui).clicked() {
                close = true;
            }
        });
        if valid && d.new_name.is_none() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            send = true;
        }
    });
    if send {
        let sub = d.chosen.trim().to_string();
        if app.s.library_last_sub != sub {
            app.s.library_last_sub = sub.clone();
            app.save_settings();
        }
        for p in &d.paths {
            app.send_now(p, &sub);
        }
        return;
    }
    if close || modal.should_close() {
        return;
    }
    app.send_dlg = Some(d);
}
