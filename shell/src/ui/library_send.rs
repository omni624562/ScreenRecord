//! 傳到媒體庫：像檔案總管一樣一層層點進媒體庫（例如 NAS 的 video）選資料夾；設定「自動傳到」也用同一個視窗。
//! 以及清單上「已在媒體庫」的標註要用的索引（整個媒體庫的檔名與大小，背景讀、記起來）。

use super::theme::{self, Btn, Icon};
use super::{dialogs, UiApp};
use eframe::egui::{self, vec2, Align, Id, Layout, RichText};
use screenrecorder_core::jellyfin;
use screenrecorder_core::{tr, trf};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// 選好資料夾之後做什麼
enum Mode {
    /// 把這些檔案傳過去
    Send(Vec<String>),
    /// 設成「自動傳到」的資料夾
    PickAuto,
}

/// 選媒體庫資料夾的視窗
pub struct SendDlg {
    mode: Mode,
    /// 現在在哪一層（「family/2024」；空的 = 最上層）
    cwd: String,
    /// 這一層底下的資料夾；None = 還在讀（NAS 可能要一下子）
    subs: Option<Vec<String>>,
    /// 第幾次換資料夾（慢的讀取結果回來時，已經換到別層就不用）
    seq: u64,
    /// 使用者自己點過資料夾了（之後找到「已經在哪裡」時不跳過去）
    moved: bool,
    /// 新增資料夾的名稱（Some = 正在輸入）
    new_name: Option<String>,
    /// 已經在媒體庫的哪些位置（同名同大小；最上層是空字串）；None = 還在找
    already: Option<Vec<String>>,
}

/// 媒體庫的索引：小寫檔名 → (在哪個位置（「family/2024」）, 大小)
pub struct LibIndex {
    root: String,
    at: Instant,
    map: HashMap<String, Vec<(String, u64)>>,
}

/// 索引多久重讀一次（別的電腦、Jellyfin 也可能放檔案進去）
const INDEX_TTL: Duration = Duration::from_secs(120);

/// 顯示用的位置：「video › family › 2024」（最上層是媒體庫資料夾的名稱）
pub fn place_label(app: &UiApp, sub: &str) -> String {
    let root = app.library_folder().unwrap_or_default();
    std::iter::once(root.as_str()).chain(sub.split('/').filter(|s| !s.is_empty())).collect::<Vec<_>>().join(" › ")
}

impl UiApp {
    /// 「傳到媒體庫」：先選要放進哪個資料夾（從上次用的那一層開始；已經傳過的跳到它在的那一層）
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
        let files: Vec<std::path::PathBuf> = paths.iter().map(std::path::PathBuf::from).collect();
        let last = self.s.library_last_sub.clone();
        self.open_picker(Mode::Send(paths), last);
        // 整個媒體庫找一遍：已經在哪裡（NAS 很大時要一下子，先讓人選）
        self.spawn(async move { tokio::task::spawn_blocking(move || jellyfin::library_places(&root, &files)).await.unwrap_or_default() }, |app, places| {
            let Some(d) = app.send_dlg.as_mut() else { return };
            // 只傳一支、而且已經在某個資料夾，使用者還沒自己點：跳到它在的那一層（再傳一次就是更新那一份）
            let jump = match places.as_slice() {
                [only] if !d.moved => Some(only.clone()),
                _ => None,
            };
            d.already = Some(places);
            if let Some(sub) = jump {
                app.picker_go(sub, false);
            }
        });
    }

    /// 選「自動傳到」的資料夾（設定頁）
    pub fn pick_auto_folder(&mut self) {
        let cur = self.s.after_copy_sub.clone();
        self.open_picker(Mode::PickAuto, cur);
    }

    fn open_picker(&mut self, mode: Mode, start: String) {
        self.send_dlg = Some(SendDlg { mode, cwd: String::new(), subs: None, seq: 0, moved: false, new_name: None, already: None });
        self.picker_go(jellyfin::normalize_subfolder(&start), false);
    }

    /// 換到這一層並讀它底下的資料夾；這一層不在了（被刪掉、改名）就回到最上層
    fn picker_go(&mut self, sub: String, by_user: bool) {
        let root = self.s.after_copy_to.clone();
        let Some(d) = self.send_dlg.as_mut() else { return };
        d.seq += 1;
        d.moved |= by_user;
        d.cwd = sub.clone();
        d.subs = None;
        d.new_name = None;
        let seq = d.seq;
        let dir = jellyfin::library_dest(&root, &sub);
        self.spawn(async move { tokio::task::spawn_blocking(move || std::path::Path::new(&dir).is_dir().then(|| jellyfin::subfolders(&dir))).await.ok().flatten() }, move |app, subs| {
            let Some(d) = app.send_dlg.as_mut().filter(|d| d.seq == seq) else { return };
            match subs {
                Some(subs) => d.subs = Some(subs),
                None if !d.cwd.is_empty() => app.picker_go(String::new(), false),
                // 連最上層都讀不到（NAS 沒開）：還是可以傳，傳的時候會寫原因
                None => d.subs = Some(vec![]),
            }
        });
    }

    /// 在現在這一層新增資料夾，建好就進去
    fn picker_mkdir(&mut self, name: String) {
        let Some(d) = self.send_dlg.as_ref() else { return };
        let sub = if d.cwd.is_empty() { name.trim().to_string() } else { format!("{}/{}", d.cwd, name.trim()) };
        let dir = jellyfin::library_dest(&self.s.after_copy_to, &sub);
        self.spawn(async move { tokio::task::spawn_blocking(move || std::fs::create_dir_all(&dir).map_err(|e| e.to_string())).await.unwrap_or_else(|e| Err(e.to_string())) }, move |app, r| match r {
            Ok(()) => app.picker_go(sub, true),
            Err(e) => app.toast(trf!("沒有建立資料夾：{e}", "Couldn't create the folder: {e}"), true),
        });
    }

    /// 真的傳過去（sub：媒體庫的哪個資料夾，「family/2024」，空的 = 最上層）
    pub fn send_now(&mut self, path: &str, sub: &str) {
        let Some(_) = self.library_folder() else { return };
        let jf = self.jellyfin();
        let dest = jellyfin::library_dest(&self.s.after_copy_to, sub);
        let file = std::path::PathBuf::from(path);
        self.toast(trf!("正在傳到「{}」：{}", "Sending to “{}”: {}", place_label(self, sub), dialogs::file_name(path)), false);
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

    /// 剪輯、加速版另存好時自動傳：放到「自動傳到」的那個資料夾
    pub fn send_auto(&mut self, path: &str) {
        let sub = self.s.after_copy_sub.clone();
        self.send_now(path, &sub);
    }

    /// 媒體庫的索引（整個媒體庫的檔名、大小）；還沒讀過或太舊時在背景重讀，這次先回傳舊的或 None
    fn lib_index(&mut self) -> Option<&LibIndex> {
        let root = self.s.after_copy_to.trim().to_string();
        if root.is_empty() {
            return None;
        }
        let fresh = self.lib_index.as_ref().is_some_and(|i| i.root == root && i.at.elapsed() < INDEX_TTL);
        if !fresh && !self.lib_index_loading {
            self.lib_index_loading = true;
            let r = root.clone();
            self.spawn(async move { tokio::task::spawn_blocking(move || (jellyfin::library_index(&r), r)).await.ok() }, |app, got| {
                app.lib_index_loading = false;
                if let Some((map, root)) = got {
                    app.lib_index = Some(LibIndex { root, at: Instant::now(), map });
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
        // 改過封面、影片資訊只透過 Jellyfin 更新的：本機的大小和媒體庫那一份不同，也算在媒體庫裡
        self.lib_index()
            .map(|i| {
                i.map.iter().flat_map(|(name, at)| at.iter().flat_map(move |(_, size)| std::iter::once(*size).chain(jellyfin::linked_sizes(name, *size)).map(move |s| (name.clone(), s)))).collect()
            })
            .unwrap_or_default()
    }

    /// 這個檔案在媒體庫的哪裡（同名同大小）；回傳顯示用的位置（「video › family › 2024」）。
    /// 本來就放在媒體庫裡的檔案、還沒讀到索引時回傳 None
    pub fn library_where(&mut self, path: &str, size: u64) -> Option<Vec<String>> {
        let root = self.s.after_copy_to.trim().to_string();
        if root.is_empty() || jellyfin::in_library(&root, std::path::Path::new(path)) {
            return None;
        }
        let name = std::path::Path::new(path).file_name()?.to_string_lossy().to_lowercase();
        let subs: Vec<String> = self.lib_index()?.map.get(&name)?.iter().filter(|(_, s)| jellyfin::same_copy(&name, size, *s)).map(|(sub, _)| sub.clone()).collect();
        let hits: Vec<String> = subs.iter().map(|s| place_label(self, s)).collect();
        (!hits.is_empty()).then_some(hits)
    }
}

pub fn show(app: &mut UiApp, ctx: &egui::Context) {
    let Some(mut d) = app.send_dlg.take() else { return };
    let mut close = false;
    let mut done = false;
    let mut go: Option<String> = None;
    let mut mkdir: Option<String> = None;
    let modal = egui::Modal::new(Id::new("send-library")).frame(theme::modal_frame(ctx)).show(ctx, |ui| {
        ui.set_width(500.0);
        let p = theme::pal(ui);
        let title = match &d.mode {
            Mode::Send(paths) if paths.len() == 1 => tr!("傳到媒體庫", "Send to library").to_string(),
            Mode::Send(paths) => trf!("傳 {} 支到媒體庫", "Send {} files to the library", paths.len()),
            Mode::PickAuto => tr!("自動傳到哪個資料夾", "Folder for automatic sends").to_string(),
        };
        ui.label(RichText::new(title).font(theme::font_bold(16.0)));
        ui.add_space(4.0);
        ui.label(RichText::new(tr!("點資料夾進去，到了要放的那一層再按下面的按鈕", "Click a folder to open it; when you're in the right one, use the button below")).color(p.muted));
        ui.add_space(8.0);
        // 目前位置：video › family › 2024，點前面的回到那一層
        let parts: Vec<String> = d.cwd.split('/').filter(|s| !s.is_empty()).map(String::from).collect();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(2.0, 4.0);
            let root = app.library_folder().unwrap_or_default();
            if Btn::new(&root).icon(Icon::Folder).small().ghost().selected(parts.is_empty()).show(ui).clicked() && !parts.is_empty() {
                go = Some(String::new());
            }
            for (i, part) in parts.iter().enumerate() {
                ui.label(theme::muted(ui, "›"));
                let last = i + 1 == parts.len();
                if Btn::new(part).small().ghost().selected(last).show(ui).clicked() && !last {
                    go = Some(parts[..=i].join("/"));
                }
            }
        });
        ui.add_space(6.0);
        // 這一層底下的資料夾
        egui::Frame::new().fill(p.surface2).corner_radius(egui::CornerRadius::same(8)).inner_margin(8.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical().max_height(220.0).auto_shrink([false, true]).show(ui, |ui| match &d.subs {
                None => {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(14.0));
                        ui.label(theme::muted(ui, tr!("正在讀取資料夾…", "Reading folders…")));
                    });
                }
                Some(subs) if subs.is_empty() => {
                    ui.label(theme::muted(ui, tr!("這裡沒有子資料夾", "No subfolders here")));
                }
                Some(subs) => {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                        for s in subs {
                            if Btn::new(s).icon(Icon::Folder).small().show(ui).clicked() {
                                go = Some(if d.cwd.is_empty() { s.clone() } else { format!("{}/{s}", d.cwd) });
                            }
                        }
                    });
                }
            });
        });
        ui.add_space(6.0);
        match &mut d.new_name {
            None => {
                if Btn::new(tr!("＋ 在這裡新增資料夾", "+ New folder here")).small().ghost().enabled(d.subs.is_some()).show(ui).clicked() {
                    d.new_name = Some(String::new());
                }
            }
            Some(name) => {
                let ok = !name.trim().is_empty() && !name.contains(['/', '\\']) && jellyfin::valid_subfolder(name);
                ui.horizontal(|ui| {
                    let r = ui.add(egui::TextEdit::singleline(name).hint_text(tr!("新資料夾的名稱", "New folder name")).desired_width(240.0));
                    r.request_focus();
                    let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if (Btn::new(tr!("建立", "Create")).small().enabled(ok).show(ui).clicked() || enter) && ok {
                        mkdir = Some(name.trim().to_string());
                    }
                    if Btn::new(tr!("取消", "Cancel")).small().ghost().show(ui).clicked() {
                        go = Some(d.cwd.clone());
                    }
                });
                if !name.trim().is_empty() && !ok {
                    ui.label(RichText::new(tr!("名稱不能有 \\ / : * ? \" < > | 這些符號", "The name can't contain \\ / : * ? \" < > |")).color(p.rec).font(theme::font(12.0)));
                }
            }
        }
        if let (Mode::Send(_), Some(already)) = (&d.mode, &d.already) {
            if !already.is_empty() {
                ui.add_space(8.0);
                let at = already.iter().map(|s| place_label(app, s)).collect::<Vec<_>>().join(tr!("、", ", "));
                let text = if already.iter().any(|s| s.eq_ignore_ascii_case(&d.cwd)) {
                    trf!("已經在「{at}」裡了，傳到這裡會換成這台電腦的版本", "Already in “{at}”; sending here replaces it with the version on this PC")
                } else {
                    trf!("已經在「{at}」裡了，傳到這裡會多一份", "Already in “{at}”; sending here adds another copy")
                };
                ui.label(RichText::new(text).color(p.warn).font(theme::font(12.5)));
            }
        }
        ui.add_space(12.0);
        let here = parts.last().cloned().unwrap_or_else(|| app.library_folder().unwrap_or_default());
        ui.allocate_ui_with_layout(vec2(ui.available_width(), 34.0), Layout::right_to_left(Align::Center), |ui| {
            let label = match d.mode {
                Mode::Send(_) => trf!("傳到「{here}」", "Send to “{here}”"),
                Mode::PickAuto => trf!("自動傳到「{here}」", "Auto-send to “{here}”"),
            };
            if Btn::new(label).primary().enabled(d.subs.is_some() && d.new_name.is_none()).show(ui).clicked() {
                done = true;
            }
            if Btn::new(tr!("取消", "Cancel")).ghost().show(ui).clicked() {
                close = true;
            }
        });
    });
    let cwd = d.cwd.clone();
    let mode = std::mem::replace(&mut d.mode, Mode::PickAuto);
    if done {
        match mode {
            Mode::Send(paths) => {
                if app.s.library_last_sub != cwd {
                    app.s.library_last_sub = cwd.clone();
                    app.save_settings();
                }
                for p in &paths {
                    app.send_now(p, &cwd);
                }
            }
            Mode::PickAuto => {
                app.s.after_copy_sub = cwd;
                app.save_settings();
            }
        }
        return;
    }
    if close || (modal.should_close() && d.new_name.is_none()) {
        return;
    }
    d.mode = mode;
    app.send_dlg = Some(d);
    if let Some(name) = mkdir {
        app.picker_mkdir(name);
    } else if let Some(sub) = go {
        app.picker_go(sub, true);
    }
}
