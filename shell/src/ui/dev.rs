//! 開發用（只在 debug 版）：自動開啟某個畫面、模擬操作並截圖，在 Linux 的 Xvfb 上檢查介面。
//! - SCREENRECORDER_DEV：要開啟的畫面（library、shots、changelog、notes（講稿小視窗）、schedule（排程錄影）、after:<路徑>（錄完自動處理）、follow:<標題>（只錄這個視窗）、followfake:<標題>（模擬已選好視窗）、record:<秒>（錄幾秒）、replay:<保留秒數>,<幾秒後存下>[,<再幾秒停止>]（重播緩衝）、export:<路徑>、edit:<路徑>、tags:<路徑>（影片資訊）、send:<路徑>|<路徑>（傳到媒體庫：選資料夾）、compress:<路徑>（壓縮）、merge:<路徑>|<路徑>（合併）、view:<路徑>、play:<路徑>（在主畫面播放）、whatsnew（新功能介紹）、about（關於）、monitors（模擬兩個螢幕）、unplug（模擬拔掉外接螢幕）、winoff:<x>,<y>（把視窗移到那裡，2 秒後拉回看得到的地方）、snip:<PNG>（用這張圖當凍結的桌面開啟框選截圖）、snipregion:<PNG>（同上，只選範圍）、stepsreview:<PNG>,<PNG>（整理步驟截圖）、hotkeys（模擬已登記快速鍵）、fakerec:<0|1>（模擬錄影中、有錄麥克風、音量條；1 = 暫停、麥克風靜音、有一則警告）、fakestate:<countdown|stopping>（模擬倒數、處理中）、toast:<文字>（顯示一則提示）、fakedone:<路徑>（模擬剛錄好這支）、fakejob:<路徑>（模擬加速版剛做好）、jfinfo:<編號>（Jellyfin 影片資訊）、jfface:<編號>|<圖片>（Jellyfin 人物照片）、update:<版本>（模擬有新版本）、cameras:<名稱>,<名稱>（模擬找到的攝影機）、settings:<general|record|audio|shots|save|after|keys|advanced|components>、ed:<剪輯視窗指令>、tour:<first|editor|shot|library>[,<第幾步>]（教學）），以 ; 分隔
//! - SCREENRECORDER_INPUT：開啟後依序模擬的操作，以 ; 分隔：
//!   wait:毫秒、click:x,y（可加 ,ctrl、,shift）、dclick:x,y（按兩下）、rclick:x,y（右鍵）、move:x,y、drag:x0,y0,x1,y1、wheel:x,y,dy（加 ,ctrl = Ctrl+滾輪）、key:Space（可加 ctrl+、alt+、shift+）、hold:Space / release:Space（按住、放開）、type:文字、shot:路徑
//! - SCREENRECORDER_SHOT：最後截圖存檔的路徑（存好後結束）；SCREENRECORDER_SHOT_AFTER：開始後幾毫秒截圖（預設 3000）

use super::{EntryAction, UiApp};
use eframe::egui::{self, pos2, Event, Key, Modifiers, PointerButton, Pos2};
use screenrecorder_core::tr;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// 一格要送出的輸入，或等待 / 截圖
enum Step {
    Events(Vec<Event>),
    Wait(Duration),
    Shot(String),
}

#[derive(Default)]
pub struct Dev {
    started: Option<Instant>,
    done_actions: bool,
    /// 剪輯視窗開啟後才執行的指令（ed:tab=ann、ed:sel=3、ed:seek=2.5、ed:tool=emoji）
    editor_cmds: Vec<String>,
    script: VecDeque<Step>,
    script_ready: Option<Instant>,
    wait_until: Option<Instant>,
    /// 等待中的截圖（依序對應收到的截圖）
    shots: VecDeque<(String, bool)>,
    final_requested: bool,
    /// update:<版本> 模擬的新版本（啟動時的檢查會蓋掉，所以每一格都重新設定）
    fake_update: Option<screenrecorder_core::types::UpdateInfo>,
    /// fakerec:<0|1> 模擬錄影中（有錄麥克風；1 = 靜音中），檢查錄影面板
    fake_rec: Option<bool>,
    /// fakestate:countdown / stopping：模擬錄影的其他階段（看膠囊在倒數、處理中的樣子）
    fake_state: Option<screenrecorder_core::types::RecorderState>,
    /// fakedone:<路徑> 模擬剛錄好這支（控制列的「錄影已儲存」、最近錄影的藍框）
    fake_done: Option<String>,
    /// fakejob:<路徑> 模擬 1.15× 加速版剛做好（控制列的「加速版做好了」）
    fake_job: Option<String>,
}

fn button(p: Pos2, pressed: bool, modifiers: Modifiers) -> Event {
    Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers }
}

fn parse_script(s: &str) -> VecDeque<Step> {
    let mut out = VecDeque::new();
    let nums = |v: &str| v.split(',').filter_map(|n| n.trim().parse::<f32>().ok()).collect::<Vec<_>>();
    for item in s.split(';').map(str::trim).filter(|a| !a.is_empty()) {
        let (k, v) = item.split_once(':').unwrap_or((item, ""));
        match k {
            "wait" => out.push_back(Step::Wait(Duration::from_millis(v.parse().unwrap_or(300)))),
            "shot" => out.push_back(Step::Shot(v.to_string())),
            "click" => {
                // click:x,y（後面加 ,ctrl 或 ,shift = 按住 Ctrl / Shift 點）
                let n = nums(v);
                let p = pos2(n[0], n[1]);
                let m = if v.ends_with(",ctrl") {
                    Modifiers::CTRL | Modifiers::COMMAND
                } else if v.ends_with(",shift") {
                    Modifiers::SHIFT
                } else {
                    Modifiers::NONE
                };
                out.push_back(Step::Events(vec![Event::PointerMoved(p)]));
                out.push_back(Step::Events(vec![Event::ModifiersChanged(m), button(p, true, m)]));
                out.push_back(Step::Events(vec![button(p, false, m)]));
                out.push_back(Step::Events(vec![Event::ModifiersChanged(Modifiers::NONE)]));
                out.push_back(Step::Wait(Duration::from_millis(200)));
            }
            "rclick" => {
                let n = nums(v);
                let p = pos2(n[0], n[1]);
                let b = |pressed| Event::PointerButton { pos: p, button: PointerButton::Secondary, pressed, modifiers: Modifiers::NONE };
                out.push_back(Step::Events(vec![Event::PointerMoved(p)]));
                out.push_back(Step::Events(vec![b(true)]));
                out.push_back(Step::Events(vec![b(false)]));
                out.push_back(Step::Wait(Duration::from_millis(200)));
            }
            "dclick" => {
                // 按兩下：同一格送出，Xvfb 上一格很慢，分開送會超過按兩下的間隔
                let n = nums(v);
                let p = pos2(n[0], n[1]);
                out.push_back(Step::Events(vec![Event::PointerMoved(p)]));
                out.push_back(Step::Events(vec![button(p, true, Modifiers::NONE), button(p, false, Modifiers::NONE), button(p, true, Modifiers::NONE), button(p, false, Modifiers::NONE)]));
                out.push_back(Step::Wait(Duration::from_millis(200)));
            }
            "move" => {
                let n = nums(v);
                out.push_back(Step::Events(vec![Event::PointerMoved(pos2(n[0], n[1]))]));
            }
            "drag" => {
                let n = nums(v);
                let (a, b) = (pos2(n[0], n[1]), pos2(n[2], n[3]));
                // 有 ctrl 時按住 Ctrl 拖曳（直到放開）
                let m = if v.contains(",ctrl") { Modifiers::CTRL | Modifiers::COMMAND } else { Modifiers::NONE };
                out.push_back(Step::Events(vec![Event::PointerMoved(a)]));
                out.push_back(Step::Events(vec![Event::ModifiersChanged(m), button(a, true, m)]));
                for i in 1..=8 {
                    out.push_back(Step::Events(vec![Event::PointerMoved(a + (b - a) * (i as f32 / 8.0))]));
                }
                // drag:x1,y1,x2,y2,hold：不放開（截圖看拖曳中的畫面），之後用 drag:x2,y2,x2,y2 放開
                if !v.contains(",hold") {
                    out.push_back(Step::Events(vec![button(b, false, m), Event::ModifiersChanged(Modifiers::NONE)]));
                }
                out.push_back(Step::Wait(Duration::from_millis(200)));
            }
            "mouseup" => {
                let n = nums(v);
                out.push_back(Step::Events(vec![button(pos2(n[0], n[1]), false, Modifiers::NONE), Event::ModifiersChanged(Modifiers::NONE)]));
            }
            "wheel" => {
                // wheel:x,y,dy（後面加 ,ctrl = 按住 Ctrl 轉滾輪）
                let n = nums(v);
                let p = pos2(n[0], n[1]);
                let modifiers = if v.ends_with(",ctrl") { Modifiers::CTRL | Modifiers::COMMAND } else { Modifiers::NONE };
                out.push_back(Step::Events(vec![Event::PointerMoved(p)]));
                out.push_back(Step::Events(vec![Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, n[2]), phase: egui::TouchPhase::Move, modifiers }]));
                out.push_back(Step::Wait(Duration::from_millis(100)));
            }
            // hold:Space / release:Space：按住、放開一個鍵（例如按住空白鍵拖曳）
            "hold" | "release" => {
                if let Some(key) = Key::from_name(v) {
                    out.push_back(Step::Events(vec![Event::Key { key, physical_key: None, pressed: k == "hold", repeat: false, modifiers: Modifiers::NONE }]));
                }
            }
            "key" => {
                // 修飾鍵：ctrl+、alt+、shift+ 可組合，例如 ctrl+alt+F9
                let (mut m, mut name) = (Modifiers::NONE, v);
                loop {
                    if let Some(n) = name.strip_prefix("shift+") {
                        (m, name) = (m | Modifiers::SHIFT, n);
                    } else if let Some(n) = name.strip_prefix("ctrl+") {
                        // Windows 上 Ctrl 同時是 command（egui 的快速鍵用 command 判斷）
                        (m, name) = (m | Modifiers::CTRL | Modifiers::COMMAND, n);
                    } else if let Some(n) = name.strip_prefix("alt+") {
                        (m, name) = (m | Modifiers::ALT, n);
                    } else {
                        break;
                    }
                }
                if let Some(key) = Key::from_name(name) {
                    let ev = |pressed| Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: m };
                    out.push_back(Step::Events(vec![ev(true)]));
                    out.push_back(Step::Events(vec![ev(false)]));
                }
            }
            "type" => out.push_back(Step::Events(vec![Event::Text(v.to_string())])),
            _ => {}
        }
    }
    out
}

/// 送出測試步驟的下一步（每一格一步）
pub fn input(app: &mut UiApp, ctx: &egui::Context, raw: &mut egui::RawInput) {
    let Some(ready) = app.dev.script_ready else {
        return;
    };
    if Instant::now() < ready || app.dev.wait_until.is_some_and(|t| Instant::now() < t) {
        return;
    }
    app.dev.wait_until = None;
    match app.dev.script.pop_front() {
        Some(Step::Events(evs)) => raw.events.extend(evs),
        Some(Step::Wait(d)) => app.dev.wait_until = Some(Instant::now() + d),
        Some(Step::Shot(path)) => {
            app.dev.shots.push_back((path, false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            app.dev.wait_until = Some(Instant::now() + Duration::from_millis(300));
        }
        None => {}
    }
}

/// 讀到新的狀態後：換成模擬的錄影狀態（fakerec）
pub fn fake_status(app: &mut UiApp) {
    if let Some(m) = app.dev.fake_rec {
        let r = &mut app.status.recorder;
        (r.state, r.mic_muted, r.recorded_ms, r.markers) = (screenrecorder_core::types::RecorderState::Recording, Some(m), 83_000, 1);
        (r.bytes, r.out_width, r.out_height, r.fps) = (6_330_000, 1920, 1080, 30.0);
        // 音量條：跟著時間跳動
        let t = app.dev.started.map_or(0.0, |s| s.elapsed().as_secs_f32());
        r.levels = Some((Some(0.05 + 0.04 * (t * 3.0).sin().abs()), Some(0.2 + 0.15 * (t * 5.0).sin().abs())));
        if let Some(st) = app.dev.fake_state {
            r.state = st;
        }
        // 靜音的那種也當作暫停中、有一則警告（看暫停、靜音、事件紀錄的樣子）
        if m {
            r.state = screenrecorder_core::types::RecorderState::Paused;
            if r.log.is_empty() {
                r.log.push(screenrecorder_core::types::LogEntry {
                    t: screenrecorder_core::paths::now_ms(),
                    level: screenrecorder_core::types::LogLevel::Warn,
                    text: "麥克風中斷：先以靜音代替並嘗試重新連線".into(),
                });
            }
        }
    }
    if let Some(path) = app.dev.fake_job.clone() {
        use screenrecorder_core::types::{ExportKind, ExportState, ExportStatus};
        let output = path.replace(".mp4", "_1.15x.mp4");
        app.status.export = Some(ExportStatus {
            id: 7,
            kind: ExportKind::Speed,
            state: ExportState::Done,
            source: path,
            output,
            speed: 1.15,
            progress: 1.0,
            expected_sec: 30.0,
            elapsed_ms: 9000,
            eta_sec: None,
            bytes: Some(11_400_000),
            message: None,
        });
    }
    if let Some(path) = app.dev.fake_done.clone() {
        let bytes = std::fs::metadata(&path).map(|m| m.len()).ok();
        let r = &mut app.status.recorder;
        r.result = Some(screenrecorder_core::types::RecordingResult { ok: true, path: Some(path), frames: 180, video_sec: 6.0, bytes, message: String::new(), parts_dir: None, silent_audio: 0 });
    }
}

pub fn tick(app: &mut UiApp, ctx: &egui::Context) {
    if let Some(u) = &app.dev.fake_update {
        app.env.update = Some(u.clone());
    }
    if app.dev.started.is_none() {
        // 測試用：SCREENRECORDER_THEME=light / dark
        match std::env::var("SCREENRECORDER_THEME").as_deref() {
            Ok("light") => ctx.set_theme(egui::ThemePreference::Light),
            Ok("dark") => ctx.set_theme(egui::ThemePreference::Dark),
            _ => {}
        }
        // 測試用：SCREENRECORDER_WINSIZE=1880x1000 換視窗大小（看寬螢幕的樣子）
        if let Some((w, h)) = std::env::var("SCREENRECORDER_WINSIZE").ok().and_then(|v| v.split_once('x').and_then(|(w, h)| Some((w.parse::<f32>().ok()?, h.parse::<f32>().ok()?)))) {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(w, h)));
        }
        app.dev.script = parse_script(&std::env::var("SCREENRECORDER_INPUT").unwrap_or_default());
    }
    let start = *app.dev.started.get_or_insert_with(Instant::now);
    if !app.dev.done_actions && app.env_ready && start.elapsed() > Duration::from_millis(1200) {
        app.dev.done_actions = true;
        if let Ok(list) = std::env::var("SCREENRECORDER_DEV") {
            for a in list.split(';').map(str::trim).filter(|a| !a.is_empty()) {
                match a.split_once(':') {
                    Some(("export", p)) => app.act_path(EntryAction::Export, p.to_string()),
                    Some(("edit", p)) => app.act_path(EntryAction::Edit, p.to_string()),
                    Some(("tags", p)) => app.act_path(EntryAction::Tags, p.to_string()),
                    Some(("send", p)) => app.send_many_to_library(p.split('|').map(String::from).collect()),
                    Some(("compress", p)) => app.act_path(EntryAction::Compress, p.to_string()),
                    // 合併對話框：路徑用 | 分開
                    Some(("merge", ps)) => {
                        app.library = Some(super::library_dialog::LibraryDialog::new(super::library_dialog::Kind::Video));
                        super::library_dialog::open_merge(app, ps.split('|').map(String::from).collect());
                    }
                    Some(("view", p)) => app.act_path(EntryAction::Play, p.to_string()),
                    Some(("play", p)) => app.act_path(EntryAction::PlayHere, p.to_string()),
                    Some(("ed", c)) => app.dev.editor_cmds.push(c.to_string()),
                    _ if a == "library" => app.library = Some(super::library_dialog::LibraryDialog::new(super::library_dialog::Kind::Video)),
                    _ if a == "shots" => app.library = Some(super::library_dialog::LibraryDialog::new(super::library_dialog::Kind::Shot)),
                    _ if a == "changelog" => app.changelog_open = true,
                    _ if a == "notes" => super::notes::toggle(app),
                    _ if a == "schedule" => super::schedule::open(app),
                    // 只錄標題含有這段文字的視窗
                    Some(("follow", t)) => {
                        if let Some(w) = screenrecorder_core::winui::app_windows().into_iter().find(|w| w.title.contains(t)) {
                            app.s.source_type = super::settings::SourceType::Region;
                            app.s.region = screenrecorder_core::types::Rect { x: w.rect.x, y: w.rect.y, width: w.rect.width & !1, height: w.rect.height & !1 };
                            app.s.follow_window = Some(super::settings::FollowWindow { id: w.id, title: w.title.clone() });
                        }
                    }
                    // 模擬已選好一個視窗（測試版面用，沒有真的視窗）
                    Some(("followfake", t)) => {
                        app.s.source_type = super::settings::SourceType::Region;
                        app.s.follow_window = Some(super::settings::FollowWindow { id: Default::default(), title: t.to_string() });
                    }
                    // 用目前的設定錄幾秒後停止
                    Some(("record", secs)) => {
                        let secs: u64 = secs.parse().unwrap_or(5);
                        let config = app.s.record_config(&app.env);
                        let core = app.core.clone();
                        app.spawn(
                            async move {
                                screenrecorder_core::actions::record_start(&core, config).await?;
                                tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
                                core.recorder.stop(None).await.map(|_| ())
                            },
                            |app, r: screenrecorder_core::Result<()>| {
                                if let Err(e) = r {
                                    app.toast(e.message().to_string(), true);
                                }
                            },
                        );
                    }
                    // 重播緩衝：replay:保留秒數,幾秒後存下[,存下後幾秒停止]
                    Some(("replay", v)) => {
                        let n: Vec<u64> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                        let (keep, save_at, stop_after) = (n.first().copied().unwrap_or(30), n.get(1).copied().unwrap_or(15), n.get(2).copied().unwrap_or(3));
                        let config = screenrecorder_core::types::RecordConfig { replay_sec: keep as u32, ..app.s.record_config(&app.env) };
                        let core = app.core.clone();
                        app.spawn(
                            async move {
                                core.replay_start(config).await?;
                                tokio::time::sleep(std::time::Duration::from_secs(save_at)).await;
                                let res = core.replay_save(false).await?;
                                screenrecorder_core::info!("[dev] 存下重播：{}", res.message);
                                tokio::time::sleep(std::time::Duration::from_secs(stop_after)).await;
                                core.recorder.stop(None).await.map(|_| ())
                            },
                            |app, r: screenrecorder_core::Result<()>| {
                                if let Err(e) = r {
                                    app.toast(e.message().to_string(), true);
                                }
                            },
                        );
                    }
                    Some(("after", p)) => {
                        let core = app.core.clone();
                        app.spawn(core.after_record(p.to_string()), |_, _| {});
                    }
                    Some(("notes", c)) => super::notes::dev(app, c),
                    Some(("tour", c)) => super::tour::dev(app, c),
                    Some(("toast", t)) => app.toast(t.to_string(), false),
                    Some(("fakedone", p)) => app.dev.fake_done = Some(p.to_string()),
                    Some(("fakejob", p)) => app.dev.fake_job = Some(p.to_string()),
                    // Jellyfin 人物照片：jfface:<影片編號>|<圖片檔>（用這張圖當影片畫面）
                    Some(("jfface", arg)) => {
                        let (id, img) = arg.split_once('|').unwrap_or((arg, ""));
                        let user = Some(app.s.jellyfin_user.clone()).filter(|u| !u.is_empty()).unwrap_or_else(|| "u".into());
                        if let Some(pm) = std::fs::read(img).ok().and_then(|b| screenrecorder_core::picture::decode_pixmap(&b)) {
                            super::jellyfin_face::open(app, pm, id.to_string(), user, vec![]);
                        }
                    }
                    // Jellyfin 影片資訊（人物、標籤的常用）
                    Some(("jfinfo", id)) => {
                        let user = Some(app.s.jellyfin_user.clone()).filter(|u| !u.is_empty()).unwrap_or_else(|| "u".into());
                        super::jellyfin_edit::open(app, id.to_string(), user, id.to_string(), vec![], vec![])
                    }
                    _ if a.starts_with("fakerec:") => app.dev.fake_rec = Some(a.ends_with('1')),
                    Some(("fakestate", st)) => {
                        app.dev.fake_rec = Some(false);
                        app.dev.fake_state = match st {
                            "countdown" => Some(screenrecorder_core::types::RecorderState::Countdown),
                            "stopping" => Some(screenrecorder_core::types::RecorderState::Stopping),
                            _ => None,
                        };
                    }
                    _ if a == "about" => app.about_open = true,
                    _ if a == "whatsnew" => {
                        app.whats_new_checked = true;
                        app.whats_new_open = true;
                    }
                    _ if a == "jellyfin" => super::library_dialog::open_jellyfin(app),
                    _ if a == "monitors" => fake_monitors(app),
                    _ if a == "unplug" => fake_unplug(app),
                    // 把操作視窗移到 x,y（例如螢幕外），2 秒後拉回看得到的地方（測試螢幕改變時的處理）
                    Some(("winoff", v)) => {
                        let n: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                        if let [x, y] = n[..] {
                            app.ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos2(x, y)));
                            app.spawn(tokio::time::sleep(Duration::from_secs(2)), |_, _| screenrecorder_core::main_window::keep_on_screen());
                        }
                    }
                    // 名稱很長的音訊裝置（檢查設定視窗不會被撐寬）
                    _ if a == "mics" => fake_mics(app),
                    // 模擬系統匣已登記快速鍵（Linux 上沒有系統匣）
                    _ if a == "hotkeys" => {
                        app.env.hotkeys = Some(screenrecorder_core::types::HotkeyStatus {
                            record: true,
                            pause: true,
                            shot: true,
                            snip: false,
                            mark: true,
                            pen: true,
                            replay: true,
                            mute: true,
                            zoom: true,
                            text: true,
                        })
                    }
                    Some(("snip", p)) => fake_snip(app, p),
                    // Windows：用這張圖當凍結的桌面開啟原生的框選畫面（在 wine 上測試），選好後照工具列的動作處理
                    #[cfg(windows)]
                    Some(("nativesnip", p)) => native_snip(app, p, screenrecorder_core::snip_win::Mode::Shot),
                    #[cfg(windows)]
                    Some(("nativeregion", p)) => native_snip(app, p, screenrecorder_core::snip_win::Mode::Record),
                    // 延遲截圖（倒數的提示；Windows 以外沒有提示）
                    Some(("delayshot", sec)) => {
                        let (core, config, sec) = (app.core.clone(), app.s.record_config(&app.env), sec.parse().unwrap_or(3));
                        app.spawn(async move { core.snip_delayed(&config, sec).await }, |app, r| match r {
                            Ok(Some(s)) => app.toast(format!("[dev] {}", s.path), false),
                            Ok(None) => app.toast("[dev] 取消", false),
                            Err(e) => app.toast(e.message().to_string(), true),
                        });
                    }
                    // 長截圖進行中的提示（Windows）：在 (300, 250)–(700, 520) 旁邊顯示 6 秒
                    #[cfg(windows)]
                    Some(("longbubble", _)) => {
                        std::thread::spawn(|| {
                            use screenrecorder_core::bubble::Content;
                            use screenrecorder_core::bubble_win::{Bubble, Options};
                            let b = Bubble::show(Content::text("長截圖中・已接 0 段・按 Esc 停止"), Options { clickable: false, esc: false, near: Some((300, 250, 700, 520)), bottom: true, on: None });
                            for n in 1..=6 {
                                std::thread::sleep(std::time::Duration::from_secs(1));
                                b.set(Content::text(format!("長截圖中・已接 {n} 段・按 Esc 停止")));
                            }
                        });
                    }
                    // 螢幕畫筆（Windows）
                    #[cfg(windows)]
                    _ if a == "pen" => screenrecorder_core::screen_pen_win::toggle(),
                    // 步驟截圖（Windows）：開始，畫面下方有小工具列
                    #[cfg(windows)]
                    _ if a == "steps" => {
                        let dir = app.s.record_config(&app.env).shots_dir();
                        if let Err(e) = app.core.steps_start(&dir) {
                            app.toast(e.message().to_string(), true);
                        }
                    }
                    // 整理步驟：用這幾張圖（, 分隔）當截好的步驟
                    Some(("stepsreview", list)) => fake_steps(app, list),
                    // 只選範圍（例如錄影範圍）：選好後顯示範圍
                    Some(("snipregion", p)) => {
                        if let Some(src) = fake_source(app, p) {
                            super::snip::open(
                                app,
                                ctx,
                                src,
                                Some(Box::new(|app: &mut UiApp, r: Option<screenrecorder_core::types::Rect>| match r {
                                    Some(r) => app.toast(format!("[dev] 範圍 ({}, {}) {}×{}", r.x, r.y, r.width, r.height), false),
                                    None => app.toast("[dev] 取消", false),
                                })),
                            );
                        }
                    }
                    Some(("cameras", list)) => app.cameras = Some(list.split(',').map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).collect()),
                    Some(("update", v)) => {
                        app.dev.fake_update = Some(screenrecorder_core::types::UpdateInfo {
                            version: v.to_string(),
                            url: "https://github.com/omni624562/ScreenRecord/releases/latest".into(),
                            published_at: None,
                            download_url: Some(format!("https://github.com/omni624562/ScreenRecord/releases/download/v{v}/ScreenRecorder.exe")),
                            sha256: Some("0".repeat(64)),
                            size: Some(15_340_544),
                        })
                    }
                    Some(("settings", p)) => {
                        use super::settings_dialog::{open, Page};
                        let page = match p {
                            "general" => Page::General,
                            "audio" => Page::Audio,
                            "shots" => Page::Shots,
                            "save" => Page::Save,
                            "after" => Page::After,
                            "keys" => Page::Keys,
                            "advanced" => Page::Advanced,
                            "components" => Page::Components,
                            _ => Page::Record,
                        };
                        open(app, page);
                    }
                    _ => {}
                }
            }
        }
    }
    if let Some(ed) = &mut app.editor {
        for c in std::mem::take(&mut app.dev.editor_cmds) {
            ed.dev(&c);
        }
    }
    // 開啟畫面後再開始模擬操作（剪輯視窗要等它開啟、解出畫面）
    if app.dev.script_ready.is_none() && app.dev.done_actions && !app.dev.script.is_empty() {
        let editing = std::env::var("SCREENRECORDER_DEV").is_ok_and(|v| v.contains("edit:"));
        if !editing || app.editor.is_some() {
            app.dev.script_ready = Some(Instant::now() + Duration::from_millis(2500));
        }
    }
    if let Ok(path) = std::env::var("SCREENRECORDER_SHOT") {
        let after = std::env::var("SCREENRECORDER_SHOT_AFTER").ok().and_then(|v| v.parse().ok()).unwrap_or(3000);
        let script_done = app.dev.script.is_empty() && app.dev.wait_until.is_none();
        if !app.dev.final_requested && script_done && start.elapsed() > Duration::from_millis(after) {
            app.dev.final_requested = true;
            app.dev.shots.push_back((path, true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
    }
    let shots: Vec<_> = ctx.input(|i| i.raw.events.iter().filter_map(|e| if let egui::Event::Screenshot { image, .. } = e { Some(image.clone()) } else { None }).collect());
    for img in shots {
        let Some((path, last)) = app.dev.shots.pop_front() else {
            continue;
        };
        let mut pm = tiny_skia::Pixmap::new(img.size[0] as u32, img.size[1] as u32).unwrap();
        pm.data_mut().copy_from_slice(img.as_raw());
        let _ = pm.save_png(&path);
        if last {
            std::process::exit(0);
        }
    }
    ctx.request_repaint_after(Duration::from_millis(30));
}

/// 模擬兩個 1920×1080 的螢幕（Linux 上沒有螢幕資訊時檢查單一螢幕的版面）
/// 假的步驟截圖：把圖複製到暫存資料夾的「教學_dev」，開啟整理視窗
fn fake_steps(app: &mut UiApp, list: &str) {
    use screenrecorder_core::steps::{Draft, Step};
    let dir = std::env::temp_dir().join(format!("steps-dev-{}", std::process::id()));
    let name = "教學_dev".to_string();
    let _ = std::fs::create_dir_all(dir.join(&name));
    let windows = ["設定", "檔案總管 - 下載", "", "記事本"];
    let mut steps = vec![];
    for (i, src) in list.split(',').filter(|p| !p.is_empty()).enumerate() {
        let image = format!("{name}/step_{:02}.png", i + 1);
        if std::fs::copy(src, dir.join(&image)).is_ok() {
            steps.push(Step { time: format!("14:30:{:02}", i * 4), window: windows[i % windows.len()].to_string(), image, text: String::new() });
        }
    }
    app.core.steps_review(Draft { dir, name, date: "2026/10/13 14:30".into(), steps });
}

fn fake_monitors(app: &mut UiApp) {
    use screenrecorder_core::types::{MonitorInfo, Rect};
    let m = |i: u32, x: i32| MonitorInfo {
        id: format!("0:{i}"),
        adapter: 0,
        output: i,
        adapter_name: "GPU".into(),
        device_name: format!("\\\\.\\DISPLAY{}", i + 1),
        display_number: i + 1,
        x,
        y: 0,
        width: 1920,
        height: 1080,
        primary: i == 0,
        rotation: 1,
        key: format!("\\\\?\\DISPLAY#FAKE{i}"),
        name: if i == 0 { "Built-in".into() } else { "DELL U2415".into() },
    };
    app.env.monitors = vec![m(0, 0), m(1, 1920)];
    app.env.desktop = Rect { x: 0, y: 0, width: 3840, height: 1080 };
    app.s.fix_monitor(&app.env);
}

/// 模擬拔掉外接螢幕：先選第二個螢幕、講稿放在它上面，再只留下第一個（和偵測到螢幕改變時一樣處理）
fn fake_unplug(app: &mut UiApp) {
    fake_monitors(app);
    let ext = app.env.monitors[1].clone();
    app.s.source_type = super::settings::SourceType::Monitor;
    app.s.monitor_id = Some(ext.id.clone());
    app.s.monitor_key = ext.key.clone();
    let mut env = app.env.clone();
    env.monitors.truncate(1);
    env.desktop = screenrecorder_core::types::Rect { x: 0, y: 0, width: 1920, height: 1080 };
    app.apply_displays(env);
}

fn fake_mics(app: &mut UiApp) {
    use screenrecorder_core::types::AudioDevice;
    let d = |id: &str, name: &str, is_default: bool| AudioDevice { id: id.into(), name: name.into(), is_default };
    app.env.audio.render = Some(tr!("耳機 (JBL Tune 720BT Hands-Free AG Audio 立體聲)", "Headphones (JBL Tune 720BT Hands-Free AG Audio Stereo)").into());
    app.env.audio.captures = vec![
        d("m1", tr!("麥克風排列 (適用於數位麥克風的 Intel® 智慧型音效技術)", "Microphone Array (Intel® Smart Sound Technology for Digital Microphones)"), true),
        d("m2", tr!("耳機 (JBL Tune 720BT Hands-Free AG Audio)", "Headset (JBL Tune 720BT Hands-Free AG Audio)"), false),
        d("m3", "USB Audio Device", false),
    ];
}

/// 用一張圖當凍結的桌面開啟框選截圖（Linux 上無法真的截下桌面）；模擬兩個視窗供點選
fn fake_snip(app: &mut UiApp, png: &str) {
    if let Some(src) = fake_source(app, png) {
        app.core.snip_offer(src);
    }
}

/// 原生的框選畫面（Windows）：用一張圖當凍結的桌面；截圖照工具列選的動作存檔、複製或釘在桌面，選範圍時顯示範圍
#[cfg(windows)]
fn native_snip(app: &mut UiApp, png: &str, mode: screenrecorder_core::snip_win::Mode) {
    use screenrecorder_core::snip_win::Mode;
    let Some(src) = fake_source(app, png) else { return };
    let (core, instant) = (app.core.clone(), app.s.snip_instant);
    app.spawn(
        async move {
            let s2 = src.clone();
            let picked = tokio::task::spawn_blocking(move || {
                let pm = tiny_skia::Pixmap::decode_png(&std::fs::read(&s2.path).ok()?).ok()?;
                let desk = screenrecorder_core::types::Rect { width: pm.width() as i32, height: pm.height() as i32, ..s2.desktop };
                screenrecorder_core::snip_win::select(pm.data(), desk, s2.windows.clone(), mode, instant)
            })
            .await
            .ok()
            .flatten();
            let msg = match picked {
                None => "[dev] 取消".to_string(),
                Some((r, a)) if mode == Mode::Record => format!("[dev] 範圍 ({}, {}) {}×{} {a:?}", r.x, r.y, r.width, r.height),
                Some((r, a)) => match core.snip_finish(&src, r, a).await {
                    Ok(shot) => format!("[dev] {a:?} {}×{} {}", r.width, r.height, shot.map(|s| s.path).unwrap_or_default()),
                    Err(e) => format!("[dev] {a:?} 失敗：{}", e.message()),
                },
            };
            screenrecorder_core::info!("{msg}");
            msg
        },
        |app, msg| app.toast(msg, false),
    );
}

/// 用一張圖當凍結的桌面（模擬兩個視窗）
fn fake_source(app: &mut UiApp, png: &str) -> Option<screenrecorder_core::app::SnipSource> {
    use screenrecorder_core::types::Rect;
    let tmp = std::env::temp_dir().join("ScreenRecorder-dev-snip.png");
    std::fs::copy(png, &tmp).ok()?;
    let desktop = if app.env.desktop.width > 0 { app.env.desktop } else { Rect { x: 0, y: 0, width: 1280, height: 900 } };
    Some(screenrecorder_core::app::SnipSource {
        path: tmp,
        desktop,
        monitors: app.env.monitors.clone(),
        windows: vec![Rect { x: 100, y: 120, width: 500, height: 300 }, Rect { x: 0, y: 0, width: 900, height: 700 }],
        shot_dir: app.s.shots_dir(&app.env),
    })
}
