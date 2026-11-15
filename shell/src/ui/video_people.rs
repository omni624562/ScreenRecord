//! 影片裡的人：用相片分頁取了名的人，在一支影片裡每隔幾秒取一格畫面找臉、認人（core::faces::scan_video），
//! 都在這台電腦上做，不上傳。這台電腦的錄影、Jellyfin 串流的影片都可以。
//!
//! - 檢視器播放列的「人物」：還沒找過時按「找出這支影片裡的人」，在背景找（一次一支，可以取消，關掉檢視器也繼續）
//! - 找到的存在設定資料夾的 video-people.json（這台電腦的影片用路徑、大小、修改時間當鍵，Jellyfin 的用影片編號），
//!   下次開啟直接列出每個人出現的時間，按了跳過去

use super::UiApp;
use screenrecorder_core::faces::{self, VideoPeople};
use screenrecorder_core::{tr, trf};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[derive(Default)]
pub struct State {
    /// 找過的影片（第一次用到時讀檔）
    store: Option<HashMap<String, VideoPeople>>,
    job: Option<Job>,
    /// 人物功能開著、模型在（每幾秒看一次）
    ready: Option<(bool, Instant)>,
}

struct Job {
    key: String,
    progress: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
}

/// 檢視器要畫的狀態
#[derive(Debug, Clone, Default, PartialEq)]
pub enum View {
    /// 沒開啟人物功能、沒有模型：不顯示按鈕
    #[default]
    Off,
    /// 還沒找過
    Ready,
    /// 正在找這一支（0～1）
    Running(f32),
    /// 正在找另一支
    Busy,
    Done(VideoPeople),
}

/// 這支影片的鍵：Jellyfin 的用影片編號，這台電腦的用路徑、大小、修改時間（檔案換了就重新找）
pub fn key(path: &str, bytes: u64, mtime: f64, remote_id: Option<&str>) -> String {
    match remote_id {
        Some(id) => format!("jf:{id}"),
        None => format!("{}|{bytes}|{}", path.to_lowercase(), mtime as i64),
    }
}

pub fn view(app: &mut UiApp, key: &str) -> View {
    let st = &mut app.video_people;
    if st.ready.is_none_or(|(_, at)| at.elapsed().as_secs() >= 5) {
        st.ready = Some((app.s.photo_faces && faces::models_ready(), Instant::now()));
    }
    if !st.ready.is_some_and(|(ok, _)| ok) {
        return View::Off;
    }
    if let Some(j) = &st.job {
        return if j.key == key { View::Running(j.progress.load(Ordering::Relaxed) as f32 / 1000.0) } else { View::Busy };
    }
    let store = st.store.get_or_insert_with(|| faces::load_video_people(&faces::video_people_file()));
    store.get(key).cloned().map_or(View::Ready, View::Done)
}

/// 在背景找這支影片裡的人
pub fn start(app: &mut UiApp, key: String, src: String, duration: f64) {
    if app.video_people.job.is_some() {
        return;
    }
    let Some(ffmpeg) = app.core.ffmpeg_path() else {
        app.toast(tr!("找不到 FFmpeg，沒辦法讀影片", "FFmpeg wasn't found, so the video can't be read"), true);
        return;
    };
    let (progress, stop) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicBool::new(false)));
    app.video_people.job = Some(Job { key: key.clone(), progress: progress.clone(), stop: stop.clone() });
    app.spawn(
        async move {
            tokio::task::spawn_blocking(move || -> Result<VideoPeople, String> {
                let people = faces::load_index(&faces::index_file()).named_centers();
                if people.is_empty() {
                    return Err(tr!("還沒有取了名的人：先在檔案庫「相片」分頁的「人物」幫他們取名", "Nobody is named yet. Name people first in People on the library's Photos tab").into());
                }
                let found = faces::scan_video(&ffmpeg, &src, duration, &people, &progress, &stop)?;
                Ok(VideoPeople { step: faces::video_step(duration), people: found })
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        },
        move |app, r| {
            let canceled = app.video_people.job.take().is_some_and(|j| j.stop.load(Ordering::Relaxed));
            match r {
                Ok(vp) => {
                    let names: Vec<&str> = vp.people.iter().map(|(n, _)| n.as_str()).collect();
                    let msg = if names.is_empty() {
                        tr!("這支影片裡沒有找到取了名的人", "No named people were found in this video").to_string()
                    } else {
                        trf!("影片裡找到：{}", "Found in the video: {}", names.join(tr!("、", ", ")))
                    };
                    let store = app.video_people.store.get_or_insert_with(|| faces::load_video_people(&faces::video_people_file()));
                    store.insert(key, vp);
                    let all = store.clone();
                    app.spawn(async move { tokio::task::spawn_blocking(move || faces::save_video_people(&faces::video_people_file(), &all)).await.ok() }, |_, _| {});
                    app.toast(msg, false);
                }
                Err(_) if canceled => {}
                Err(e) => app.toast(e, true),
            }
        },
    );
}

pub fn cancel(app: &mut UiApp) {
    if let Some(j) = &app.video_people.job {
        j.stop.store(true, Ordering::Relaxed);
    }
}

/// 忘掉這支影片找到的（重新找之前）
pub fn forget(app: &mut UiApp, key: &str) {
    if let Some(s) = app.video_people.store.as_mut() {
        s.remove(key);
    }
}
