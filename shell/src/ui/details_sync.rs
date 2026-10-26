//! 本機錄影和 Jellyfin 上同名影片的影片資訊雙向同步（見 core::details_sync）：
//! 在哪一邊改，另一邊就跟著改；在 Jellyfin 網頁、手機改的，背景對的時候帶回本機。

use super::UiApp;
use screenrecorder_core::details_sync::{self, Common};
use screenrecorder_core::jellyfin::JfInfo;
use screenrecorder_core::{tr, trf};
use std::time::{Duration, Instant};

/// 背景全部對一次的間隔（Jellyfin 網頁、手機改的多久會帶回本機）
const EVERY: Duration = Duration::from_secs(30 * 60);

impl UiApp {
    /// 背景把本機錄影和 Jellyfin 上同名的影片資訊全部對一次。force：剛在這裡批次改過，不管上次什麼時候對過
    pub fn sync_details_all(&mut self, force: bool) {
        if self.details_sync_busy || !force && self.details_sync_at.is_some_and(|t| t.elapsed() < EVERY) {
            return;
        }
        let (Some(jf), Some(ff)) = (self.jellyfin(), self.core.ffmpeg_path()) else { return };
        self.details_sync_busy = true;
        self.details_sync_at = Some(Instant::now());
        let user = self.s.jellyfin_user.clone();
        let dirs = vec![self.s.out_dir(&self.env)];
        let subdirs = self.s.list_subdirs;
        self.spawn(
            async move {
                let dirs = screenrecorder_core::library::expand_dirs(dirs, subdirs).await;
                details_sync::sync_all(jf, user, ff, dirs).await
            },
            |app, r| {
                app.details_sync_busy = false;
                match r {
                    Ok(rep) => {
                        for p in &rep.written {
                            app.core.cache.forget(p);
                        }
                        let (n, m) = (rep.written.len(), rep.pushed);
                        if n > 0 || m > 0 {
                            app.load_recent();
                            app.toast(trf!("影片資訊已和 Jellyfin 同步：本機更新 {n} 支、Jellyfin 更新 {m} 支", "Video details synced with Jellyfin: {n} here, {m} in Jellyfin"), false);
                        }
                    }
                    Err(e) => screenrecorder_core::info!("[影片資訊同步] 沒有對成：{e}"),
                }
            },
        );
    }

    /// 本機的影片資訊存好了：Jellyfin 上同名的那一支換成一樣的（這邊是最後改的，以這邊為準）。
    /// Jellyfin 上沒有這支、或沒設定 Jellyfin 時照舊同步「複製」資料夾裡的那一份
    pub fn push_details(&mut self, path: &str) {
        let (Some(jf), Some(ff)) = (self.jellyfin(), self.core.ffmpeg_path()) else {
            return self.sync_edited(path, screenrecorder_core::jellyfin::Edit::Details);
        };
        let user = self.s.jellyfin_user.clone();
        let p = path.to_string();
        let back = path.to_string();
        self.spawn(
            async move {
                let Some(mut f) = details_sync::check(&jf, &user, &ff, &p).await? else { return Ok(None) };
                // 本機剛寫好，這邊是最後改的：照本機的改 Jellyfin
                let merged = Common::from_local(&f.local);
                f.plan.to_local = false;
                let changed = !merged.same(&Common::from_jf(&f.info, &file_name(&p)));
                details_sync::apply(&jf, &user, &ff, &p, &f, &merged).await?;
                Ok(Some(changed))
            },
            move |app, r: Result<Option<bool>, String>| match r {
                Ok(Some(true)) => app.toast(tr!("Jellyfin 上的影片資訊也更新了", "Details updated in Jellyfin too"), false),
                Ok(Some(false)) => {}
                // Jellyfin 上沒有這支：照舊同步「複製」資料夾裡的那一份
                Ok(None) => app.sync_edited(&back, screenrecorder_core::jellyfin::Edit::Details),
                // 連不到 Jellyfin（不在家）：背景對的時候會發現這邊改過，到時再補
                Err(e) => {
                    screenrecorder_core::info!("[影片資訊同步] 沒有更新到 Jellyfin：{e}");
                    app.toast(tr!("暫時沒有更新到 Jellyfin，連得到時會自動補上", "Couldn't reach Jellyfin; it will be updated automatically later"), false);
                }
            },
        );
    }

    /// Jellyfin 上的影片資訊存好了：本機同名的那一支（錄影資料夾裡只有一支時）換成一樣的
    pub fn pull_details(&mut self, file: &str, info: JfInfo) {
        let Some(ff) = self.core.ffmpeg_path() else { return };
        let dirs = vec![self.s.out_dir(&self.env)];
        let subdirs = self.s.list_subdirs;
        let file = file.to_string();
        self.spawn(
            async move {
                let dirs = screenrecorder_core::library::expand_dirs(dirs, subdirs).await;
                let want = file.to_lowercase();
                let hits: Vec<String> = dirs
                    .iter()
                    .filter_map(|d| std::fs::read_dir(d).ok())
                    .flat_map(|rd| rd.flatten())
                    .filter(|e| e.file_name().to_string_lossy().to_lowercase() == want)
                    .map(|e| e.path().display().to_string())
                    .collect();
                let [path] = hits.as_slice() else { return Ok(None) };
                if !screenrecorder_core::video_tags::can_tag(path) {
                    return Ok(None);
                }
                let local = screenrecorder_core::video_tags::read(&ff, path).await.map_err(|e| e.message().to_string())?;
                let l = Common::from_local(&local);
                let mut j = Common::from_jf(&info, &file);
                // Jellyfin 上沒標題（用檔名當的）：本機的標題留著
                if j.title.is_empty() {
                    j.title = l.title.clone();
                }
                if !j.same(&l) {
                    details_sync::write_local(Some(&ff), path, &j.to_local(&local)).await.map_err(|e| e.message().to_string())?;
                }
                details_sync::remember(&file, &j);
                Ok::<_, String>((!j.same(&l)).then(|| path.clone()))
            },
            |app, r| match r {
                Ok(Some(path)) => {
                    app.core.cache.forget(&path);
                    app.load_recent();
                    app.toast(trf!("本機的「{}」也換成一樣的影片資訊", "The copy on this PC (“{}”) was updated too", super::dialogs::file_name(&path)), false);
                }
                Ok(None) => {}
                Err(e) => app.toast(trf!("本機那一支的影片資訊沒有跟著更新：{e}", "Couldn't update the details of the copy on this PC: {e}"), true),
            },
        );
    }
}

fn file_name(path: &str) -> String {
    std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}
