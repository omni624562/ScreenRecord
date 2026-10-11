//! 單一執行個體：啟動時掃描連接埠找已在執行的螢幕錄影。
//! - 同一代（3.x 以後）：請它把操作視窗帶到前面，自己結束
//! - 舊版（1.x Bun、2.x Tauri）：請它正常結束（錄影中會先停止並儲存），再由新版接手

use crate::ipc::APP_ID;
use std::time::{Duration, Instant};

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(Duration::from_millis(800)).build()
}

/// 這個連接埠上是不是螢幕錄影；是的話回傳它的版本（3.x 在 ping 裡，舊版要另外問 /api/env）
fn probe(port: u16) -> Option<String> {
    let a = agent();
    let ping: serde_json::Value = serde_json::from_str(&a.get(&format!("http://127.0.0.1:{port}/api/ping")).call().ok()?.into_string().ok()?).ok()?;
    if ping["app"] != APP_ID {
        return None;
    }
    if let Some(v) = ping["version"].as_str() {
        return Some(v.to_string());
    }
    let env: serde_json::Value = serde_json::from_str(&a.get(&format!("http://127.0.0.1:{port}/api/env")).call().ok()?.into_string().ok()?).ok()?;
    Some(env["appVersion"].as_str().unwrap_or("").to_string())
}

/// 已在執行的螢幕錄影：（連接埠, 版本）
pub fn find_running(ports: &[u16]) -> Vec<(u16, String)> {
    std::thread::scope(|s| {
        let handles: Vec<_> = ports.iter().map(|p| s.spawn(move || probe(*p).map(|v| (*p, v)))).collect();
        handles.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
    })
}

/// 舊版（1.x、2.x）：由新版請它結束後接手
pub fn is_legacy(version: &str) -> bool {
    version.split('.').next().and_then(|m| m.parse::<u32>().ok()).is_none_or(|m| m < 3)
}

/// 找已在執行的舊版：回傳（連接埠, 版本）
pub fn find_legacy(ports: &[u16]) -> Option<(u16, String)> {
    find_running(ports).into_iter().find(|(_, v)| is_legacy(v))
}

/// 請已在執行的程式把操作視窗帶到前面
pub fn show(port: u16) -> bool {
    agent().post(&format!("http://127.0.0.1:{port}/api/show")).set("Content-Type", "application/json").send_string("{}").is_ok()
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
        let legacy = fake_legacy("2.1.0");
        let current = fake_legacy("3.0.0");
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert_eq!(find_legacy(&[closed, current, legacy]), Some((legacy, "2.1.0".to_string())));
        assert_eq!(find_legacy(&[closed, current]), None); // 同一代：帶到前面，不結束
        assert!(is_legacy("1.4.0") && is_legacy("2.0.0") && !is_legacy("3.0.0") && !is_legacy("10.1.0"));
        assert!(quit_and_wait(legacy, Duration::from_secs(5)));
        assert_eq!(find_legacy(&[legacy]), None);
    }
}
