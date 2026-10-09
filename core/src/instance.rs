//! 從 1.x 版升級：1.x 版（Bun）不在 Tauri 的單一實例機制裡，開著時新版會再多一個系統匣圖示、快捷鍵也登記不到。
//! 啟動時掃描連接埠，找到 1.x 版就請它正常結束（錄影中會先停止並儲存），再由新版接手。

use crate::server::APP_ID;
use std::time::{Duration, Instant};

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(Duration::from_millis(800)).build()
}

/// 這個連接埠上是不是螢幕錄影；是的話回傳它的版本
fn probe(port: u16) -> Option<String> {
    let a = agent();
    let ping: serde_json::Value = serde_json::from_str(&a.get(&format!("http://127.0.0.1:{port}/api/ping")).call().ok()?.into_string().ok()?).ok()?;
    if ping["app"] != APP_ID {
        return None;
    }
    let env: serde_json::Value = serde_json::from_str(&a.get(&format!("http://127.0.0.1:{port}/api/env")).call().ok()?.into_string().ok()?).ok()?;
    Some(env["appVersion"].as_str().unwrap_or("").to_string())
}

/// 找已在執行的 1.x 版：回傳（連接埠, 版本）
pub fn find_legacy(ports: &[u16]) -> Option<(u16, String)> {
    let found: Vec<(u16, String)> = std::thread::scope(|s| {
        let handles: Vec<_> = ports.iter().map(|p| s.spawn(move || probe(*p).map(|v| (*p, v)))).collect();
        handles.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
    });
    found.into_iter().find(|(_, v)| v.starts_with("1."))
}

/// 請它結束並等它關閉（錄影中要先合併，最多等 timeout）
pub fn quit_and_wait(port: u16, timeout: Duration) -> bool {
    let _ = agent().post(&format!("http://127.0.0.1:{port}/api/quit")).set("Content-Type", "application/json").send_string("{}");
    let start = Instant::now();
    while start.elapsed() < timeout {
        if probe(port).is_none() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 假的 1.x 版：回應 ping / env，收到 quit 就關閉
    fn fake_legacy(version: &'static str) -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let mut buf = [0u8; 2048];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let body = if req.starts_with("GET /api/ping") {
                    format!("{{\"app\":\"{APP_ID}\",\"pid\":1}}")
                } else if req.starts_with("GET /api/env") {
                    format!("{{\"appVersion\":\"{version}\"}}")
                } else {
                    "{\"ok\":true}".to_string()
                };
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                if req.starts_with("POST /api/quit") {
                    return; // 關閉 listener
                }
            }
        });
        port
    }

    #[test]
    fn finds_and_quits_legacy_instance() {
        let legacy = fake_legacy("1.4.0");
        let current = fake_legacy("2.0.0");
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert_eq!(find_legacy(&[closed, current, legacy]), Some((legacy, "1.4.0".to_string())));
        assert_eq!(find_legacy(&[closed, current]), None); // 2.x 不處理（由單一實例機制負責）
        assert!(quit_and_wait(legacy, Duration::from_secs(5)));
        assert_eq!(find_legacy(&[legacy]), None);
    }
}
