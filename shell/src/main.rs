//! 螢幕錄影（原速錄影 + 加速匯出）—— 單一執行檔：
//! 本機 API 伺服器、錄影、轉檔（screenrecorder-core）＋系統匣＋操作視窗（Tauri / WebView2）。
//!
//! 參數：
//!   --port <n>   指定連接埠（預設 47391，被占用時自動往後找）
//!   --no-open    啟動後不自動開啟操作視窗
//!   --tray       只常駐系統匣、不開視窗（開機自動啟動時使用）
//!   --no-tray    不建立系統匣圖示、不檢查是否已在執行（開發測試用，可與正式程式並存）
//!
//! - 只會有一個實例：再次啟動時把操作視窗帶到前面
//! - 關掉操作視窗後程式仍常駐在系統匣，從圖示選單操作或結束
//! - 操作視窗只顯示本機介面：要前往其他網址（更新頁面、FFmpeg 下載頁）時交給預設瀏覽器
//! - 視窗標題與網頁標題同步：以「螢幕錄影 v」開頭的標題辨識這個視窗（開始錄影時縮小）
#![windows_subsystem = "windows"]

use screenrecorder_core::app::App;
use screenrecorder_core::paths::{app_dir, data_dir, now_ms};
use screenrecorder_core::version::APP_VERSION;
use screenrecorder_core::{desktop, error, info, instance, job, log, selfupdate, server, tray, warn};
use std::sync::Arc;
use std::time::Duration;
use tauri::webview::NewWindowResponse;
use tauri::{AppHandle, Manager, RunEvent, Url, WebviewUrl, WebviewWindowBuilder};

const MAIN: &str = "main";
const DEFAULT_PORT: u16 = 47391;
const PORT_RANGE: u16 = 20;
/// 系統匣無法使用時：超過這麼久沒有開著的視窗、也沒在錄影 / 轉檔就自動結束
const IDLE_EXIT_MS: u64 = 5 * 60_000;

#[derive(Clone, Copy)]
struct Args {
    port: u16,
    auto_open: bool,
    no_tray: bool,
}

fn parse_args(argv: &[String]) -> Args {
    let has = |name: &str| argv.iter().any(|a| a == name);
    let port = argv
        .iter()
        .position(|a| a == "--port")
        .and_then(|i| argv.get(i + 1).cloned())
        .or_else(|| argv.iter().find_map(|a| a.strip_prefix("--port=").map(String::from)))
        .or_else(|| std::env::var("PORT").ok())
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT);
    Args { port, auto_open: !has("--no-open") && !has("--tray"), no_tray: has("--no-tray") }
}

/// 只允許本機介面（伺服器只綁 127.0.0.1）
fn is_local(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https") && matches!(url.host_str(), Some("127.0.0.1") | Some("localhost"))
}

/// 用預設瀏覽器開啟外部網址（只接受 http / https）
fn open_external(url: &Url) {
    if matches!(url.scheme(), "http" | "https") {
        desktop::open_with_explorer(url.as_str(), false);
    }
}

/// 把字串轉成 JavaScript 字串常值（避免網址內容被當成程式碼）
fn js_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into()).replace('\u{2028}', "\\u2028").replace('\u{2029}', "\\u2029")
}

/// 開啟操作視窗；已開著時帶到前面（網址不同時才換頁，例如系統匣的「更新說明」只改 #changelog）。
/// WebView2 無法使用（例如沒有安裝執行環境）時改用瀏覽器。
fn open_window(app: &AppHandle, url: &str) {
    let Some(u) = Url::parse(url).ok().filter(is_local) else { return };
    if let Some(w) = app.get_webview_window(MAIN) {
        if w.url().map(|cur| cur != u).unwrap_or(true) {
            let _ = w.eval(format!("location.href = {};", js_string(u.as_str())));
        }
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let built = WebviewWindowBuilder::new(app, MAIN, WebviewUrl::External(u))
        .title(format!("螢幕錄影 v{APP_VERSION}"))
        .inner_size(1280.0, 900.0)
        .min_inner_size(1024.0, 640.0)
        .center()
        .focused(true)
        // 介面不需要拖放檔案；關掉才不會攔截網頁裡的拖曳（剪輯視窗框選範圍）
        .disable_drag_drop_handler()
        .data_directory(data_dir().join("webview"))
        .on_document_title_changed(|w, t| {
            if !t.is_empty() {
                let _ = w.set_title(&t);
            }
        })
        // 只顯示本機介面，其他網址交給預設瀏覽器
        .on_navigation(|u| {
            if is_local(u) {
                return true;
            }
            open_external(u);
            false
        })
        .on_new_window(|u, _features| {
            open_external(&u);
            NewWindowResponse::Deny
        })
        .build();
    if let Err(e) = built {
        warn!("無法開啟操作視窗（{e}），改用瀏覽器");
        desktop::open_in_browser(url);
    }
}

/// 從 1.x 版升級：舊版還開著時請它正常結束（錄影中會先儲存），由新版接手
async fn replace_legacy(preferred: u16) {
    let mut ports: Vec<u16> = (0..PORT_RANGE).map(|i| preferred.saturating_add(i)).chain((0..PORT_RANGE).map(|i| DEFAULT_PORT + i)).collect();
    ports.sort_unstable();
    ports.dedup();
    let found = tokio::task::spawn_blocking(move || instance::find_legacy(&ports)).await.ok().flatten();
    let Some((port, version)) = found else { return };
    info!("舊版 v{version} 還在執行（連接埠 {port}），請它結束後由新版接手");
    let done = tokio::task::spawn_blocking(move || instance::quit_and_wait(port, Duration::from_secs(120))).await.unwrap_or(false);
    if !done {
        warn!("舊版 v{version} 沒有在時間內結束，系統匣可能會出現兩個圖示");
    }
}

async fn startup(core: Arc<App>, args: Args) {
    if !args.no_tray {
        replace_legacy(args.port).await;
    }

    let Some((listener, port)) = server::bind(args.port, PORT_RANGE).await else {
        error!("無法啟動網頁伺服器（{} 起的連接埠都被占用）", args.port);
        core.quit(1).await;
        return;
    };
    let url = format!("http://127.0.0.1:{port}/");
    core.set_url(url.clone());
    info!("介面網址：{url}");
    info!("記錄檔：{}", log::log_file().display());
    let c = core.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = server::serve(listener, c, port).await {
            error!("網頁伺服器停止：{e}");
        }
    });
    // 偵測 FFmpeg、螢幕與音訊裝置（要跑好幾次 ffmpeg）在背景進行，同時建立系統匣並開啟視窗；
    // 介面讀取環境資訊時會等第一次偵測完成
    let c = core.clone();
    tauri::async_runtime::spawn(async move {
        c.refresh(false).await;
        c.log_startup();
    });

    // 系統匣常駐：關掉操作視窗後程式仍在背景，從圖示選單操作或結束
    let tray_ok = !args.no_tray && tray::start(&core).await;
    info!("{}", if tray_ok { "已常駐於系統匣：右鍵點圖示可錄影或結束程式" } else { "系統匣無法使用，關閉操作視窗 5 分鐘後會自動結束" });
    if args.auto_open {
        core.open_ui(url);
    }
    if !args.no_tray {
        core.schedule_update_checks();
    }
    let c = core.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(30)).await;
        let _ = tokio::task::spawn_blocking(move || c.thumbs.prune()).await;
    });

    // 系統匣無法使用時的退路：關掉視窗後就看不到程式了，
    // 超過一段時間沒有任何頁面在輪詢狀態、而且沒在錄影 / 轉檔，就自動結束
    if !tray_ok && !args.no_tray {
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            if core.recorder.active() || core.exporter.running() {
                continue;
            }
            if now_ms().saturating_sub(core.last_seen()) > IDLE_EXIT_MS {
                info!("超過 5 分鐘沒有開著的視窗，自動結束程式");
                core.quit(0).await;
                return;
            }
        }
    }
}

fn main() {
    log::setup();
    log::install_panic_hook();
    let argv: Vec<String> = std::env::args().collect();
    // 程式內更新後由舊版啟動：等舊版結束（否則會被當成重複執行），再清掉舊版留下的檔案
    if let Some(pid) = argv.iter().find_map(|a| a.strip_prefix("--wait-pid=")).and_then(|p| p.parse::<u32>().ok()) {
        info!("已更新到 v{APP_VERSION}，等舊版結束");
        selfupdate::wait_for_exit(pid, Duration::from_secs(60));
    }
    if let Ok(exe) = std::env::current_exe() {
        selfupdate::cleanup_old(&exe);
    }
    // 本程式結束（包括當掉）時，FFmpeg 等子程序一併結束
    job::install();
    let args = parse_args(&argv);
    info!("螢幕錄影 {APP_VERSION} — 原速錄影、事後加速匯出");
    info!("程式資料夾：{}", app_dir().display());

    let mut builder = tauri::Builder::default();
    if !args.no_tray {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // 已在執行：再次啟動時開啟操作視窗（開機自動啟動的 --tray 不開）
            if argv.iter().any(|a| a == "--tray") {
                return;
            }
            if let Some(core) = app.try_state::<Arc<App>>() {
                let core = core.inner().clone();
                // 不在事件處理中直接建立視窗（Windows 上可能卡住）
                tauri::async_runtime::spawn(async move { core.open_ui(core.url()) });
            }
        }));
    }
    let app = builder
        .setup(move |app| {
            let handle = app.handle().clone();
            let core = App::new();
            app.manage(core.clone());
            let h = handle.clone();
            core.set_ui_opener(Arc::new(move |url| open_window(&h, &url)));
            let h = handle.clone();
            core.set_exiter(Arc::new(move |code| h.exit(code)));
            tauri::async_runtime::spawn(startup(core, args));
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("無法啟動程式");
    app.run(|_app, event| {
        // 關掉操作視窗後繼續常駐（只有從系統匣或介面選「結束」才真的結束）
        if let RunEvent::ExitRequested { api, code: None, .. } = event {
            api.prevent_exit();
        }
    });
}
