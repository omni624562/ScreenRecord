//! 傳到媒體庫：像檔案總管一樣一層層點進媒體庫（例如 NAS 的 video）選資料夾；設定「自動傳到」也用同一個視窗。
//! 以及清單上「已在媒體庫」的標註要用的索引（整個媒體庫的檔名與大小，背景讀、記起來）。

use super::theme::{self, Btn, Icon};
use super::{dialogs, UiApp};
use eframe::egui::{self, vec2, Align, Id, Layout, RichText};
use screenrecorder_core::{jellyfin, transfer};
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

/// 沒選媒體庫資料夾、索引改問 Jellyfin 時 LibIndex.root 的開頭（後面接 Jellyfin 的網址）
const JF_INDEX: &str = "jellyfin:";

/// 顯示用的位置：「video › family › 2024」（最上層是媒體庫資料夾的名稱）
pub fn place_label(app: &UiApp, sub: &str) -> String {
    let root = app.library_folder().unwrap_or_default();
    std::iter::once(root.as_str()).filter(|r| !r.is_empty()).chain(sub.split('/').filter(|s| !s.is_empty())).collect::<Vec<_>>().join(" › ")
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
                tr!(
                    "請先在「設定 → 存檔與分享 → Jellyfin」選 Jellyfin 媒體庫的資料夾，之後就能直接傳",
                    "Choose the Jellyfin library folder in Settings → Saving & sharing → Jellyfin first; then you can send files there"
                ),
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

    /// 真的傳過去（sub：媒體庫的哪個資料夾，「family/2024」，空的 = 最上層）。
    /// 連不到 NAS（不在家、NAS 沒開）時記進待傳清單，連上後自動補傳
    pub fn send_now(&mut self, path: &str, sub: &str) {
        let Some(_) = self.library_folder() else { return };
        let jf = self.jellyfin();
        let root = self.s.after_copy_to.clone();
        let dest = jellyfin::library_dest(&root, sub);
        let file = std::path::PathBuf::from(path);
        let place = place_label(self, sub);
        self.toast(trf!("正在傳到「{place}」：{}", "Sending to “{place}”: {}", dialogs::file_name(path)), false);
        let (key, sub) = (path.to_string(), sub.to_string());
        let (core, h264) = (self.core.clone(), self.s.library_h264);
        self.spawn(
            async move {
                if !reachable_bg(&root).await {
                    return None;
                }
                let r = send_one(&core, jf, &dest, &file, h264).await;
                // 傳到一半斷線：也算連不到
                if matches!(r, Some(Err(_))) && !reachable_bg(&root).await {
                    return None;
                }
                Some(r)
            },
            move |app, r| {
                // 媒體庫的檔案變了：標註重讀
                app.lib_index = None;
                match r {
                    None => {
                        transfer::add_pending(std::path::Path::new(&key), &sub);
                        app.toast(trf!("連不到「{place}」：已記下，連上之後會自動傳過去", "Can't reach “{place}”. It's queued and will be sent once it's reachable"), false);
                    }
                    Some(Some(Ok(msg))) => {
                        transfer::remove_pending(&key);
                        app.toast(msg, false);
                    }
                    Some(Some(Err(e))) => app.toast(e, true),
                    Some(None) => {}
                }
            },
        );
    }

    /// 待傳清單：每分鐘看一次 NAS 連不連得到，連得到就一支一支補傳（logic 每一畫格呼叫）
    pub fn flush_pending(&mut self) {
        if self.pending_busy || self.pending_check.elapsed() < Duration::from_secs(60) {
            return;
        }
        self.pending_check = Instant::now();
        let list = transfer::pending();
        let root = self.s.after_copy_to.trim().to_string();
        if list.is_empty() || root.is_empty() {
            return;
        }
        self.pending_busy = true;
        let jf = self.jellyfin();
        let (core, h264) = (self.core.clone(), self.s.library_h264);
        self.spawn(
            async move {
                let mut done: Vec<(String, Result<(), String>)> = vec![];
                if !reachable_bg(&root).await {
                    return done;
                }
                for p in list {
                    let file = std::path::PathBuf::from(&p.file);
                    if !file.is_file() {
                        // 本機的檔案已經刪掉了：不用再傳
                        done.push((p.file, Ok(())));
                        continue;
                    }
                    let r = send_one(&core, jf.clone(), &jellyfin::library_dest(&root, &p.sub), &file, h264).await;
                    let failed = matches!(r, Some(Err(_)));
                    done.push((p.file, r.unwrap_or(Ok(String::new())).map(|_| ())));
                    // 又斷線了：剩下的下次再傳
                    if failed && !reachable_bg(&root).await {
                        break;
                    }
                }
                done
            },
            |app, done| {
                app.pending_busy = false;
                let mut sent = 0;
                for (file, r) in &done {
                    match r {
                        Ok(()) => {
                            transfer::remove_pending(file);
                            sent += std::path::Path::new(file).is_file() as usize;
                        }
                        Err(e) => screenrecorder_core::warn!("[傳到資料夾] 補傳 {file} 失敗：{e}"),
                    }
                }
                if sent > 0 {
                    app.lib_index = None;
                    app.toast(trf!("Jellyfin 的資料夾連上了：已補傳 {sent} 支", "The Jellyfin folder is back: sent {sent} queued file(s)"), false);
                }
            },
        );
    }

    /// 剪輯、加速版另存好時自動傳：放到「自動傳到」的那個資料夾
    pub fn send_auto(&mut self, path: &str) {
        let sub = self.s.after_copy_sub.clone();
        self.send_now(path, &sub);
    }

    /// 誰在 Jellyfin 看過這支（名稱, 日期）；每 5 分鐘在背景重讀一次（也順便同步我的最愛）
    pub fn watched_by(&mut self, name: &str) -> Vec<(String, String)> {
        let jf = self.jellyfin();
        let fresh = self.jf_watch.as_ref().is_some_and(|(at, _)| at.elapsed() < Duration::from_secs(300));
        if jf.is_some() && !fresh && !self.jf_watch_loading {
            self.refresh_watch();
        }
        self.jf_watch.as_ref().and_then(|(_, st)| st.watched.get(&name.to_lowercase()).cloned()).unwrap_or_default()
    }

    /// 重讀 Jellyfin 的觀看紀錄與我的最愛；我的最愛有變化時本機的星號跟著改
    pub fn refresh_watch(&mut self) {
        let Some(jf) = self.jellyfin() else { return };
        self.jf_watch_loading = true;
        let user = self.s.jellyfin_user.clone();
        let dirs = vec![self.s.out_dir(&self.env)];
        let subdirs = self.s.list_subdirs;
        self.spawn(
            async move {
                let st = tokio::task::spawn_blocking(move || jf.watch_state(Some(&user).filter(|u| !u.is_empty()).map(|u| u.as_str()))).await.ok()?.ok()?;
                // 我的最愛和上次比：新加的本機也加星號，拿掉的本機也拿掉
                let mut pins: Vec<(String, bool)> = vec![];
                if st.fav_user.is_some() {
                    let (added, removed) = jellyfin::favorites_changed(&st.favorites);
                    if !added.is_empty() || !removed.is_empty() {
                        let dirs = screenrecorder_core::library::expand_dirs(dirs, subdirs).await;
                        for (path, name) in local_files(&dirs) {
                            if added.contains(&name) {
                                pins.push((path, true));
                            } else if removed.contains(&name) {
                                pins.push((path, false));
                            }
                        }
                    }
                }
                Some((st, pins))
            },
            |app, r| {
                app.jf_watch_loading = false;
                let Some((st, pins)) = r else {
                    // 連不到 Jellyfin：5 分鐘後再試
                    app.jf_watch = Some((Instant::now(), Default::default()));
                    return;
                };
                for (path, on) in &pins {
                    if app.core.pins.contains(path) != *on {
                        app.core.pins.set(path, *on);
                    }
                }
                if !pins.is_empty() {
                    if let Some(d) = app.library.as_mut() {
                        d.library_index_changed();
                    }
                }
                app.jf_watch = Some((Instant::now(), st));
            },
        );
    }

    /// 本機改了星號：Jellyfin 我的最愛也跟著改（有選「星號同步到」的帳號、Jellyfin 有這支時）
    pub fn sync_favorite(&mut self, path: &str, on: bool) {
        let user = self.s.jellyfin_user.clone();
        let Some(jf) = self.jellyfin().filter(|_| !user.is_empty()) else { return };
        let name = dialogs::file_name(path).to_string();
        self.spawn(async move { tokio::task::spawn_blocking(move || jf.set_favorite(&user, &name, on).map(|n| (n, name))).await.unwrap_or_else(|e| Err(e.to_string())) }, move |app, r| match r {
            Ok((n, name)) if n > 0 => {
                jellyfin::favorite_set(&name, on);
                if let Some((_, st)) = app.jf_watch.as_mut() {
                    if on {
                        st.favorites.insert(name.to_lowercase());
                    } else {
                        st.favorites.remove(&name.to_lowercase());
                    }
                }
            }
            Ok(_) => {}
            Err(e) => screenrecorder_core::warn!("[Jellyfin] 同步我的最愛失敗：{e}"),
        });
    }

    /// 「已在 Jellyfin」的索引從哪裡讀：選了媒體庫資料夾就讀資料夾；沒選但有設定 Jellyfin 時問 Jellyfin。都沒有時 None
    fn index_source(&self) -> Option<String> {
        let root = self.s.after_copy_to.trim();
        if !root.is_empty() {
            return Some(root.to_string());
        }
        self.jellyfin().map(|_| format!("{JF_INDEX}{}", self.s.jellyfin_url.trim()))
    }

    /// 媒體庫的索引（整個媒體庫的檔名、大小）；還沒讀過或太舊時在背景重讀，這次先回傳舊的或 None
    fn lib_index(&mut self) -> Option<&LibIndex> {
        let root = self.index_source()?;
        let fresh = self.lib_index.as_ref().is_some_and(|i| i.root == root && i.at.elapsed() < INDEX_TTL);
        if !fresh && !self.lib_index_loading {
            self.lib_index_loading = true;
            let r = root.clone();
            let jf = root.starts_with(JF_INDEX).then(|| self.jellyfin()).flatten();
            let job = move || {
                let map = match jf {
                    Some(jf) => jf.file_index().unwrap_or_else(|e| {
                        screenrecorder_core::warn!("[Jellyfin] 讀不到影片清單（已在 Jellyfin 的標籤）：{e}");
                        Default::default()
                    }),
                    None => jellyfin::library_index(&r),
                };
                (map, r)
            };
            self.spawn(async move { tokio::task::spawn_blocking(job).await.ok() }, |app, got| {
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

    /// 已經傳到媒體庫了嗎（只看已經讀好的索引，不會開始讀；還沒讀到時當作沒有）
    pub fn sent_already(&self, path: &str, size: u64) -> bool {
        let Some(root) = self.index_source() else { return false };
        let Some(index) = self.lib_index.as_ref().filter(|i| i.root == root) else { return false };
        let name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        index.map.get(&name).is_some_and(|v| v.iter().any(|(_, s)| jellyfin::same_copy(&name, size, *s)))
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
        let folder = self.s.after_copy_to.trim().to_string();
        if !folder.is_empty() && jellyfin::in_library(&folder, std::path::Path::new(path)) {
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
            Mode::Send(paths) if paths.len() == 1 => tr!("傳到 Jellyfin", "Send to Jellyfin").to_string(),
            Mode::Send(paths) => trf!("傳 {} 支到 Jellyfin", "Send {} files to Jellyfin", paths.len()),
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

/// 傳一支到媒體庫的資料夾 dest：先看畫面格式，HEVC、AV1 這類有些電視不能直接播——h264 時先轉成 H.264 再傳
/// （NAS 上那一份和本機大小不同，記下對照），不然在結果後面提醒。沒設定媒體庫時 None
async fn send_one(core: &std::sync::Arc<screenrecorder_core::app::App>, jf: Option<jellyfin::Jellyfin>, dest: &str, file: &std::path::Path, h264: bool) -> Option<Result<String, String>> {
    let path = file.display().to_string();
    // 影片資訊的標籤、有沒有章節：傳好之後設到 Jellyfin（從原檔讀，轉檔不一定留得住）
    let extras = jellyfin::Extras::read(core.ffmpeg_path().as_deref(), file).await;
    let codec =
        if screenrecorder_core::video_tags::can_tag(&path) && !path.to_ascii_lowercase().ends_with(".m4a") { screenrecorder_core::actions::tv_unfriendly_codec(core, &path).await } else { None };
    let mut send = file.to_path_buf();
    if codec.is_some() && h264 {
        match screenrecorder_core::actions::h264_for_library(core, &path).await {
            Ok(p) => send = p,
            Err(e) => return Some(Err(e.message().to_string())),
        }
    }
    let (dest2, send2) = (dest.to_string(), send.clone());
    let r = tokio::task::spawn_blocking(move || jellyfin::send_file(jf.as_ref(), &dest2, &send2, &extras)).await.ok().flatten();
    if send != file {
        // 轉好的那一份傳完就刪；NAS 上的大小和本機不同，記下是同一支
        let name = send.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if let (Ok(a), Ok(b)) = (std::fs::metadata(file), std::fs::metadata(std::path::Path::new(dest).join(&name))) {
            jellyfin::remember_link(&name, a.len(), b.len());
        }
        if let Some(dir) = send.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
    match (r, codec) {
        (Some(Ok(msg)), Some(c)) if !h264 => Some(Ok(format!(
            "{msg}{}",
            trf!(
                "（這支是 {c}：有些電視播放時 Jellyfin 要即時轉檔，可能卡頓。可以在「設定 → 存檔與分享」打開「傳的時候轉成 H.264」）",
                " (This video is {c}; some TVs make Jellyfin convert it while playing, which can stutter. Turn on “Convert to H.264 when sending” in Settings → Saving & sharing)"
            )
        ))),
        (Some(Ok(msg)), Some(_)) => Some(Ok(format!("{msg}{}", tr!("（已轉成 H.264，電視都能直接播）", " (converted to H.264 so any TV can play it)")))),
        (r, _) => r,
    }
}

/// 連得到媒體庫資料夾嗎（NAS 沒回應時可能要等好幾秒，在背景執行緒查）
async fn reachable_bg(root: &str) -> bool {
    let root = root.to_string();
    tokio::task::spawn_blocking(move || transfer::reachable(&root)).await.unwrap_or(false)
}

/// 傳到 NAS、從 Jellyfin 下載的進度（右下角）：檔名、百分比、已傳 / 全部，可以取消；
/// 傳好之後 Jellyfin 還在整理的（等它掃描到、做時間軸預覽縮圖）也列在這裡，好了才拿掉
pub fn transfer_bar(app: &mut UiApp, ctx: &egui::Context) {
    let list = transfer::active();
    let stages = transfer::stages();
    if list.is_empty() && stages.is_empty() {
        return;
    }
    let gb = |b: u64| screenrecorder_core::format::format_bytes(b);
    egui::Area::new(Id::new("transfer-bar")).anchor(egui::Align2::RIGHT_BOTTOM, vec2(-16.0, -16.0)).order(egui::Order::Foreground).show(ctx, |ui| {
        theme::modal_frame(ctx).show(ui, |ui| {
            let p = theme::pal(ui);
            ui.set_width(300.0);
            for t in &list {
                let frac = if t.total > 0 { t.done as f32 / t.total as f32 } else { 0.0 };
                ui.horizontal(|ui| {
                    let what = if t.down { tr!("下載", "Downloading") } else { tr!("傳到 Jellyfin", "Sending to Jellyfin") };
                    ui.label(RichText::new(what).font(theme::font_bold(12.5)));
                    ui.add(egui::Label::new(RichText::new(&t.name).font(theme::font(12.0)).color(p.muted)).truncate());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let tip = if t.down {
                            tr!("取消下載", "Cancel the download")
                        } else {
                            tr!("取消（Jellyfin 那邊原本的那一份不會動）", "Cancel (the copy already on Jellyfin stays)")
                        };
                        if Btn::icon_only(Icon::Close).ghost().small().tooltip(tip).show(ui).clicked() {
                            transfer::cancel(t.id);
                        }
                    });
                });
                ui.add(egui::ProgressBar::new(frac).desired_height(6.0).fill(p.accent));
                ui.label(RichText::new(format!("{:.0}%　{} / {}", frac * 100.0, gb(t.done), gb(t.total))).font(theme::font(11.5)).color(p.muted));
            }
            for (name, what) in &stages {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new(tr!("Jellyfin 整理中", "Jellyfin is processing")).font(theme::font_bold(12.5)));
                    ui.add(egui::Label::new(RichText::new(name).font(theme::font(12.0)).color(p.muted)).truncate());
                });
                ui.label(RichText::new(what).font(theme::font(11.5)).color(p.muted));
            }
        });
    });
    let _ = app;
    ctx.request_repaint_after(Duration::from_millis(if list.is_empty() { 1000 } else { 250 }));
}

/// 這些資料夾裡的影片（路徑, 小寫檔名）
fn local_files(dirs: &[String]) -> Vec<(String, String)> {
    dirs.iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flat_map(|rd| rd.flatten())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| (e.path().display().to_string(), e.file_name().to_string_lossy().to_lowercase()))
        .collect()
}
