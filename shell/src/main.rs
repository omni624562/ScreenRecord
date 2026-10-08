//! 螢幕錄影的操作視窗：用 Tauri（Windows 內建的 WebView2）顯示主程式本機伺服器提供的介面，
//! 取代原本借用 Chrome 的 app 模式視窗。
//!
//! 由主程式啟動：
//!   screenrecorder-ui.exe --url=http://127.0.0.1:5173/ --title=螢幕錄影 v1.4.0 --data-dir=<資料夾>
//!
//! - 只會有一個視窗：再次啟動時把網址交給已開著的視窗並帶到前面（例如系統匣的「更新說明」只改 #changelog）
//! - 只顯示本機介面：要前往其他網址（更新頁面、FFmpeg 下載頁）時交給預設瀏覽器
//! - 視窗標題與網頁標題同步：主程式以「螢幕錄影 v」開頭的標題辨識這個視窗（開始錄影時縮小）
#![windows_subsystem = "windows"]

use std::path::PathBuf;
use tauri::webview::NewWindowResponse;
use tauri::{AppHandle, Manager, Url, WebviewUrl, WebviewWindowBuilder};

const MAIN: &str = "main";

struct Args {
    url: Option<String>,
    title: String,
    data_dir: Option<PathBuf>,
}

fn parse_args(args: &[String]) -> Args {
    let mut a = Args { url: None, title: "螢幕錄影".into(), data_dir: None };
    for arg in args.iter().skip(1) {
        if let Some(v) = arg.strip_prefix("--url=") {
            a.url = Some(v.to_string());
        } else if let Some(v) = arg.strip_prefix("--title=") {
            a.title = v.to_string();
        } else if let Some(v) = arg.strip_prefix("--data-dir=") {
            a.data_dir = Some(PathBuf::from(v));
        }
    }
    a
}

/// 只允許本機介面（主程式只綁 127.0.0.1）
fn is_local(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https") && matches!(url.host_str(), Some("127.0.0.1") | Some("localhost"))
}

/// 用預設瀏覽器開啟外部網址（只接受 http / https）
fn open_external(url: &Url) {
    if matches!(url.scheme(), "http" | "https") {
        let _ = std::process::Command::new("explorer.exe").arg(url.as_str()).spawn();
    }
}

/// 再次啟動時：已開著的視窗改到新網址（同一頁只改 # 時不會重新載入）並帶到前面
fn show_existing(app: &AppHandle, url: Option<&str>) {
    let Some(w) = app.get_webview_window(MAIN) else { return };
    if let Some(u) = url.and_then(|u| Url::parse(u).ok()).filter(is_local) {
        // 以 JSON 字串帶入，避免網址內容被當成程式碼
        let js = format!("location.href = {};", serde_json_string(u.as_str()));
        let _ = w.eval(&js);
    }
    let _ = w.unminimize();
    let _ = w.show();
    let _ = w.set_focus();
}

/// 把字串轉成 JavaScript 字串常值
fn serde_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Args { url, title, data_dir } = parse_args(&args);
    let Some(url) = url.and_then(|u| Url::parse(&u).ok()).filter(is_local) else {
        // 沒有本機網址：不是由主程式啟動的，直接結束
        return;
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            show_existing(app, parse_args(&argv).url.as_deref());
        }))
        .setup(move |app| {
            let mut builder = WebviewWindowBuilder::new(app, MAIN, WebviewUrl::External(url))
                .title(&title)
                .inner_size(1280.0, 900.0)
                .min_inner_size(1024.0, 640.0)
                .center()
                .focused(true)
                // 介面不需要拖放檔案；關掉才不會攔截網頁裡的拖曳（剪輯視窗框選範圍）
                .disable_drag_drop_handler()
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
                });
            if let Some(dir) = data_dir.clone() {
                builder = builder.data_directory(dir);
            }
            builder.build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("無法開啟操作視窗");
}
