//! 開發用（只在 debug 版）：自動開啟某個畫面、模擬操作並截圖，在 Linux 的 Xvfb 上檢查介面。
//! - SCREENRECORDER_DEV：要開啟的畫面（library、shots、changelog、export:<路徑>、edit:<路徑>、view:<路徑>、whatsnew（新功能介紹）、monitors（模擬兩個螢幕）、snip:<PNG>（用這張圖當凍結的桌面開啟框選截圖）、hotkeys（模擬已登記快速鍵）、settings:<record|audio|save|keys|advanced>、ed:<剪輯視窗指令>），以 ; 分隔
//! - SCREENRECORDER_INPUT：開啟後依序模擬的操作，以 ; 分隔：
//!   wait:毫秒、click:x,y、rclick:x,y（右鍵）、move:x,y、drag:x0,y0,x1,y1、wheel:x,y,dy、key:Space（可加 ctrl+、alt+、shift+）、type:文字、shot:路徑
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
                let n = nums(v);
                let p = pos2(n[0], n[1]);
                out.push_back(Step::Events(vec![Event::PointerMoved(p)]));
                out.push_back(Step::Events(vec![button(p, true, Modifiers::NONE)]));
                out.push_back(Step::Events(vec![button(p, false, Modifiers::NONE)]));
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
            "move" => {
                let n = nums(v);
                out.push_back(Step::Events(vec![Event::PointerMoved(pos2(n[0], n[1]))]));
            }
            "drag" => {
                let n = nums(v);
                let (a, b) = (pos2(n[0], n[1]), pos2(n[2], n[3]));
                out.push_back(Step::Events(vec![Event::PointerMoved(a)]));
                out.push_back(Step::Events(vec![button(a, true, Modifiers::NONE)]));
                for i in 1..=8 {
                    out.push_back(Step::Events(vec![Event::PointerMoved(a + (b - a) * (i as f32 / 8.0))]));
                }
                out.push_back(Step::Events(vec![button(b, false, Modifiers::NONE)]));
                out.push_back(Step::Wait(Duration::from_millis(200)));
            }
            "wheel" => {
                let n = nums(v);
                let p = pos2(n[0], n[1]);
                out.push_back(Step::Events(vec![Event::PointerMoved(p)]));
                out.push_back(Step::Events(vec![Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, n[2]), phase: egui::TouchPhase::Move, modifiers: Modifiers::NONE }]));
                out.push_back(Step::Wait(Duration::from_millis(100)));
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

pub fn tick(app: &mut UiApp, ctx: &egui::Context) {
    if app.dev.started.is_none() {
        // 測試用：SCREENRECORDER_THEME=light / dark
        match std::env::var("SCREENRECORDER_THEME").as_deref() {
            Ok("light") => ctx.set_theme(egui::ThemePreference::Light),
            Ok("dark") => ctx.set_theme(egui::ThemePreference::Dark),
            _ => {}
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
                    Some(("view", p)) => app.act_path(EntryAction::Play, p.to_string()),
                    Some(("ed", c)) => app.dev.editor_cmds.push(c.to_string()),
                    _ if a == "library" => app.library = Some(super::library_dialog::LibraryDialog::new(super::library_dialog::Kind::Video)),
                    _ if a == "shots" => app.library = Some(super::library_dialog::LibraryDialog::new(super::library_dialog::Kind::Shot)),
                    _ if a == "changelog" => app.changelog_open = true,
                    _ if a == "whatsnew" => {
                        app.whats_new_checked = true;
                        app.whats_new_open = true;
                    }
                    _ if a == "monitors" => fake_monitors(app),
                    // 名稱很長的音訊裝置（檢查設定視窗不會被撐寬）
                    _ if a == "mics" => fake_mics(app),
                    // 模擬系統匣已登記快速鍵（Linux 上沒有系統匣）
                    _ if a == "hotkeys" => app.env.hotkeys = Some(screenrecorder_core::types::HotkeyStatus { record: true, pause: true, shot: true, snip: false, mark: true, pen: true }),
                    Some(("snip", p)) => fake_snip(app, p),
                    Some(("settings", p)) => {
                        use super::settings_dialog::{open, Page};
                        let page = match p {
                            "audio" => Page::Audio,
                            "save" => Page::Save,
                            "keys" => Page::Keys,
                            "advanced" => Page::Advanced,
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
    };
    app.env.monitors = vec![m(0, 0), m(1, 1920)];
    app.env.desktop = Rect { x: 0, y: 0, width: 3840, height: 1080 };
    app.s.fix_monitor(&app.env);
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
    use screenrecorder_core::types::Rect;
    let tmp = std::env::temp_dir().join("ScreenRecorder-dev-snip.png");
    if std::fs::copy(png, &tmp).is_err() {
        return;
    }
    let desktop = if app.env.desktop.width > 0 { app.env.desktop } else { Rect { x: 0, y: 0, width: 1280, height: 900 } };
    let src = screenrecorder_core::app::SnipSource {
        path: tmp,
        desktop,
        monitors: app.env.monitors.clone(),
        windows: vec![Rect { x: 100, y: 120, width: 500, height: 300 }, Rect { x: 0, y: 0, width: 900, height: 700 }],
        output_dir: app.s.out_dir(&app.env),
    };
    app.core.snip_offer(src);
}
