//! 螢幕錄影（原速錄影 + 加速匯出 + 剪輯）—— 單一執行檔：
//! 錄影、轉檔、剪輯（screenrecorder-core）＋系統匣＋操作視窗（egui，不需要瀏覽器或 WebView2）。
//!
//! 參數：
//!   --port <n>   控制端點的連接埠（預設 47391，被占用時自動往後找）
//!   --no-open    啟動後不自動開啟操作視窗
//!   --tray       只常駐系統匣、不開視窗（開機自動啟動時使用）
//!   --no-tray    不建立系統匣圖示、不檢查是否已在執行（開發測試用，可與正式程式並存）
//!
//! - 只會有一個實例：再次啟動時把操作視窗帶到前面；舊版（1.x、2.x）還開著時請它結束後接手
//! - 關掉操作視窗後程式仍常駐在系統匣，從圖示選單操作或結束
//! - 視窗標題以「螢幕錄影 v」開頭：開始錄影時用來找到並縮小這個視窗
#![windows_subsystem = "windows"]

mod ui;

use eframe::egui;
use screenrecorder_core::app::{App, UiPage};
use screenrecorder_core::paths::app_dir;
use screenrecorder_core::version::APP_VERSION;
use screenrecorder_core::{error, info, instance, ipc, job, log, selfupdate, tray, warn};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

const DEFAULT_PORT: u16 = 47391;
const PORT_RANGE: u16 = 20;

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

fn scan_ports(preferred: u16) -> Vec<u16> {
    let mut ports: Vec<u16> = (0..PORT_RANGE).map(|i| preferred.saturating_add(i)).chain((0..PORT_RANGE).map(|i| DEFAULT_PORT + i)).collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// 已經有一個在執行：同一代就請它把視窗帶到前面（回傳 true，自己結束）；舊版就請它結束、由這個接手
fn hand_over(args: &Args, argv: &[String]) -> bool {
    for (port, version) in instance::find_running(&scan_ports(args.port)) {
        if instance::is_legacy(&version) {
            info!("舊版 v{version} 還在執行（連接埠 {port}），請它結束後由新版接手");
            if !instance::quit_and_wait(port, Duration::from_secs(120)) {
                warn!("舊版 v{version} 沒有在時間內結束，系統匣可能會出現兩個圖示");
            }
        } else {
            // 開機自動啟動（--tray）時不開視窗
            if !argv.iter().any(|a| a == "--tray") {
                instance::show(port);
            }
            info!("v{version} 已在執行（連接埠 {port}），結束這個");
            return true;
        }
    }
    false
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
    if !args.no_tray && hand_over(&args, &argv) {
        return;
    }
    info!("螢幕錄影 {APP_VERSION} — 原速錄影、事後加速匯出");
    info!("程式資料夾：{}", app_dir().display());

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("無法建立執行環境");
    let _enter = rt.enter();
    let core = App::new();
    let open_requests = ui::OpenRequests::default();
    static CTX: OnceLock<egui::Context> = OnceLock::new();

    // 系統匣、再次啟動、更新說明：開啟視窗
    let reqs = open_requests.clone();
    core.set_ui_opener(Arc::new(move |page: UiPage| {
        reqs.push(page);
        if let Some(ctx) = CTX.get() {
            ctx.request_repaint();
        }
    }));
    core.set_exiter(Arc::new(|code| std::process::exit(code)));
    // 錄自訂範圍時，在螢幕上的範圍外圍顯示外框與控制列（已錄時間、暫停 / 繼續、停止）
    #[cfg(windows)]
    {
        use screenrecorder_core::rec_frame_win::{self, FrameCmd};
        let (c1, c2, h) = (Arc::downgrade(&core), Arc::downgrade(&core), rt.handle().clone());
        rec_frame_win::spawn(
            move || c1.upgrade()?.recorder.frame_info(),
            move |cmd| {
                let Some(c) = c2.upgrade() else { return };
                h.spawn(async move {
                    let r = &c.recorder;
                    let res = match cmd {
                        FrameCmd::Pause => r.pause().await,
                        FrameCmd::Resume => r.resume().await,
                        FrameCmd::Stop => r.stop(None).await.map(|_| ()),
                        FrameCmd::Mark => r.add_marker().map(|_| ()),
                        FrameCmd::Move(x, y) => {
                            let old = r.frame_info().map(|f| f.area);
                            r.move_region(x, y).await.map(|new| {
                                // 主畫面的範圍、「重複上次框選」也跟著移過去
                                if let Some(old) = old.filter(|o| *o != new) {
                                    c.region_moved(old, new);
                                }
                            })
                        }
                    };
                    if let Err(e) = res {
                        warn!("錄影控制列：{}", e.message());
                        c.notify("錄影控制", e.message(), true);
                    }
                });
            },
        );
    }

    // 「只錄這個視窗」：視窗移動後錄影範圍跟著移過去
    {
        let _g = rt.enter();
        core.spawn_window_follower();
    }
    // 錄影中：記下滑鼠點擊（剪輯時跟著點擊放大用），並依設定在畫面上顯示點擊與快捷鍵（會錄進影片）
    #[cfg(windows)]
    {
        let (c1, c2) = (Arc::downgrade(&core), Arc::downgrade(&core));
        screenrecorder_core::input_overlay_win::spawn(
            move || c1.upgrade()?.recorder.overlay_info(),
            move |x, y| {
                if let Some(c) = c2.upgrade() {
                    c.recorder.add_click(x, y);
                }
            },
        );
    }

    // 錄影時的攝影機小窗：在擷取範圍的角落顯示攝影機畫面（可以拖曳），直接錄進影片
    #[cfg(windows)]
    {
        let (c1, c2, c3) = (Arc::downgrade(&core), Arc::downgrade(&core), Arc::downgrade(&core));
        screenrecorder_core::camera_bubble_win::spawn(
            move || c1.upgrade()?.recorder.camera_info(),
            move || c2.upgrade()?.ffmpeg_path(),
            move |text| {
                if let Some(c) = c3.upgrade() {
                    // 錄影器的事件紀錄也會寫進記錄檔
                    c.recorder.log_warn(&text);
                    c.notify("攝影機", &text, true);
                }
            },
        );
    }

    // 控制端點：再次啟動時把視窗帶到前面、新版接手時正常結束
    if !args.no_tray {
        match ipc::bind(args.port, PORT_RANGE) {
            Some((listener, port)) => {
                let (c1, c2, rt2) = (core.clone(), core.clone(), rt.handle().clone());
                ipc::serve(
                    listener,
                    ipc::Control {
                        show: Box::new(move || c1.open_ui(UiPage::Main)),
                        quit: Box::new(move || {
                            let c = c2.clone();
                            rt2.spawn(async move { c.quit(0).await });
                        }),
                    },
                );
                info!("控制端點：127.0.0.1:{port}");
            }
            None => error!("無法啟動控制端點（{} 起的連接埠都被占用）", args.port),
        }
    }
    info!("記錄檔：{}", log::log_file().display());

    // 偵測 FFmpeg、螢幕與音訊裝置（要跑好幾次 ffmpeg）在背景進行，同時建立系統匣並開啟視窗
    let c = core.clone();
    rt.spawn(async move {
        c.refresh(false).await;
        c.log_startup();
    });
    // 系統匣常駐：關掉操作視窗後程式仍在背景，從圖示選單操作或結束
    let tray_ok = !args.no_tray && rt.block_on(tray::start(&core));
    info!("{}", if tray_ok { "已常駐於系統匣：右鍵點圖示可錄影或結束程式" } else { "系統匣無法使用：關掉操作視窗就會結束程式" });
    if !args.no_tray {
        core.schedule_update_checks();
    }
    let c = core.clone();
    rt.spawn(async move {
        tokio::time::sleep(Duration::from_secs(30)).await;
        let _ = tokio::task::spawn_blocking(move || c.thumbs.prune()).await;
    });

    // 沒有系統匣時一定要開視窗（否則看不到程式）
    let visible = args.auto_open || !tray_ok;
    let icon = screenrecorder_core::icon::render_icon(64, screenrecorder_core::icon::IconState::Idle);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(format!("螢幕錄影 v{APP_VERSION}"))
            .with_inner_size([1280.0, 900.0])
            .with_min_inner_size([1024.0, 640.0])
            .with_visible(visible)
            .with_icon(egui::IconData { rgba: icon, width: 64, height: 64 })
            .with_app_id("screenrecorder"),
        centered: true,
        ..Default::default()
    };
    let handle = rt.handle().clone();
    let result = eframe::run_native(
        "screenrecorder",
        options,
        Box::new(move |cc| {
            let _ = CTX.set(cc.egui_ctx.clone());
            Ok(Box::new(ui::UiApp::new(cc, core, handle, open_requests, tray_ok, visible)))
        }),
    );
    if let Err(e) = result {
        error!("無法開啟操作視窗：{e}");
    }
}
