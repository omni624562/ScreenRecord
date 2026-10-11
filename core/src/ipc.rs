//! 單一執行個體的控制端點：只綁 127.0.0.1 的小型 HTTP 伺服器，讓再次啟動的程式（或新版）找到正在執行的這一個：
//! - GET  /api/ping：確認是螢幕錄影（app、pid、version）
//! - GET  /api/env：版本（與 1.x / 2.x 相容的探測方式）
//! - POST /api/show：把操作視窗帶到前面
//! - POST /api/quit：正常結束（錄影中會先停止並儲存；新版接手時使用）
//!
//! 安全：Host 必須是 127.0.0.1 / localhost（擋 DNS rebinding）；POST 必須是 JSON 且沒有 Origin（擋網頁的跨站要求）。

use crate::version::APP_VERSION;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

pub const APP_ID: &str = "screen-recorder";

/// 控制端點收到要求時要做的事
pub struct Control {
    pub show: Box<dyn Fn() + Send + Sync>,
    pub quit: Box<dyn Fn() + Send + Sync>,
}

/// 綁定連接埠：preferred 被占用時往後找
pub fn bind(preferred: u16, range: u16) -> Option<(TcpListener, u16)> {
    (preferred..preferred.saturating_add(range)).find_map(|p| {
        let l = TcpListener::bind(("127.0.0.1", p)).ok()?;
        let port = l.local_addr().ok()?.port();
        Some((l, port))
    })
}

/// 在背景執行緒處理要求（程式結束前一直執行）
pub fn serve(listener: TcpListener, control: Control) {
    let control = Arc::new(control);
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    std::thread::Builder::new()
        .name("ipc".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let c = control.clone();
                std::thread::spawn(move || {
                    let _ = handle(stream, port, &c);
                });
            }
        })
        .expect("無法建立控制端點的執行緒");
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

fn read_request(stream: &TcpStream) -> std::io::Result<Request> {
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    let mut r = BufReader::new(stream);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    let mut headers = Vec::new();
    let mut len = 0usize;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            let (k, v) = (k.trim().to_string(), v.trim().to_string());
            if k.eq_ignore_ascii_case("content-length") {
                len = v.parse().unwrap_or(0);
            }
            headers.push((k, v));
        }
        if headers.len() > 64 {
            break;
        }
    }
    // 讀掉內容（不使用；最多 4 KB）
    let mut body = vec![0u8; len.min(4096)];
    let _ = std::io::Read::read_exact(&mut r, &mut body);
    Ok(Request { method, path, headers })
}

fn respond(mut stream: &TcpStream, status: &str, body: &str) -> std::io::Result<()> {
    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}", body.len())?;
    stream.flush()
}

/// 只接受本機程式的要求（回傳 None = 允許）
fn reject(req: &Request, port: u16) -> Option<&'static str> {
    let host = req.header("host").unwrap_or("");
    if host != format!("127.0.0.1:{port}") && host != format!("localhost:{port}") {
        return Some("403 Forbidden");
    }
    if req.method == "POST" {
        if req.header("origin").is_some() || req.header("sec-fetch-site").is_some_and(|s| s != "none") {
            return Some("403 Forbidden");
        }
        if !req.header("content-type").unwrap_or("").contains("application/json") {
            return Some("415 Unsupported Media Type");
        }
    }
    None
}

fn handle(stream: TcpStream, port: u16, c: &Control) -> std::io::Result<()> {
    let req = read_request(&stream)?;
    if let Some(status) = reject(&req, port) {
        return respond(&stream, status, "{\"ok\":false}");
    }
    let path = req.path.split('?').next().unwrap_or("");
    match (req.method.as_str(), path) {
        ("GET", "/api/ping") => respond(&stream, "200 OK", &serde_json::json!({ "app": APP_ID, "pid": std::process::id(), "version": APP_VERSION }).to_string()),
        ("GET", "/api/env") => respond(&stream, "200 OK", &serde_json::json!({ "appVersion": APP_VERSION }).to_string()),
        ("POST", "/api/show") => {
            (c.show)();
            respond(&stream, "200 OK", "{\"ok\":true}")
        }
        ("POST", "/api/quit") => {
            respond(&stream, "200 OK", "{\"ok\":true}")?;
            (c.quit)();
            Ok(())
        }
        _ => respond(&stream, "404 Not Found", "{\"ok\":false}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn agent() -> ureq::Agent {
        ureq::AgentBuilder::new().timeout(Duration::from_secs(3)).build()
    }

    #[test]
    fn ping_show_quit_and_guards() {
        let shows = Arc::new(AtomicUsize::new(0));
        let quits = Arc::new(AtomicUsize::new(0));
        let (listener, port) = bind(0, 1).or_else(|| bind(40000, 2000)).unwrap();
        let (s, q) = (shows.clone(), quits.clone());
        serve(
            listener,
            Control {
                show: Box::new(move || {
                    s.fetch_add(1, Ordering::SeqCst);
                }),
                quit: Box::new(move || {
                    q.fetch_add(1, Ordering::SeqCst);
                }),
            },
        );
        let base = format!("http://127.0.0.1:{port}");
        let ping: serde_json::Value = serde_json::from_str(&agent().get(&format!("{base}/api/ping")).call().unwrap().into_string().unwrap()).unwrap();
        assert_eq!(ping["app"], APP_ID);
        assert_eq!(ping["version"], APP_VERSION);
        let env: serde_json::Value = serde_json::from_str(&agent().get(&format!("{base}/api/env")).call().unwrap().into_string().unwrap()).unwrap();
        assert_eq!(env["appVersion"], APP_VERSION);

        let post = |path: &str| agent().post(&format!("{base}{path}")).set("Content-Type", "application/json").send_string("{}").is_ok();
        assert!(post("/api/show"));
        assert_eq!(shows.load(Ordering::SeqCst), 1);
        assert!(post("/api/quit"));
        // quit 先回覆再結束（程式會直接結束）：等它執行
        let t0 = std::time::Instant::now();
        while quits.load(Ordering::SeqCst) == 0 && t0.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(quits.load(Ordering::SeqCst), 1);

        // 網頁送來的要求（有 Origin）、不是 JSON、錯誤的 Host：拒絕
        let code = |r: Result<ureq::Response, ureq::Error>| match r {
            Ok(r) => r.status(),
            Err(ureq::Error::Status(c, _)) => c,
            Err(e) => panic!("{e}"),
        };
        assert_eq!(code(agent().post(&format!("{base}/api/quit")).set("Content-Type", "application/json").set("Origin", "http://evil.example").send_string("{}")), 403);
        assert_eq!(code(agent().post(&format!("{base}/api/quit")).set("Content-Type", "text/plain").send_string("{}")), 415);
        assert_eq!(code(agent().get(&format!("{base}/api/ping")).set("Host", "evil.example").call()), 403);
        assert_eq!(code(agent().get(&format!("{base}/nope")).call()), 404);
        assert_eq!(quits.load(Ordering::SeqCst), 1);
    }
}
