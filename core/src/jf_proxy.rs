//! Jellyfin 串流的本機轉送：交給 FFmpeg、mpv 的網址是 http://127.0.0.1:埠/隨機碼/…，不帶 API 金鑰。
//! FFmpeg 的命令列別的程式看得到、出錯時也會整行寫進記錄檔；金鑰只留在這個程式裡，由這裡加在標頭（X-Emby-Token）送給 Jellyfin。
//! 只聽本機、網址要有這次執行才有的隨機碼；支援 Range（FFmpeg 跳著看時用）。開不了埠時退回原本帶金鑰的網址。

use std::hash::{BuildHasher, Hasher};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

struct Relay {
    port: u16,
    token: String,
    /// 轉送到哪些伺服器（網址, 金鑰）：網址裡的編號就是位置
    servers: Mutex<Vec<(String, String)>>,
}

fn random_token() -> String {
    // RandomState 每次都由作業系統的亂數重新播種
    let a = std::collections::hash_map::RandomState::new().build_hasher().finish();
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    h.write_u32(std::process::id());
    format!("{a:016x}{:016x}", h.finish())
}

fn relay() -> Option<&'static Relay> {
    static R: OnceLock<Option<Relay>> = OnceLock::new();
    R.get_or_init(|| {
        let l = TcpListener::bind(("127.0.0.1", 0)).map_err(|e| crate::warn!("[Jellyfin] 無法開啟本機轉送，串流網址改帶金鑰：{e}")).ok()?;
        let port = l.local_addr().ok()?.port();
        std::thread::Builder::new()
            .name("jellyfin-relay".into())
            .spawn(move || {
                for c in l.incoming().flatten() {
                    let _ = std::thread::Builder::new().name("jellyfin-relay-conn".into()).spawn(move || serve(c));
                }
            })
            .ok()?;
        Some(Relay { port, token: random_token(), servers: Mutex::new(vec![]) })
    })
    .as_ref()
}

/// 給 FFmpeg、mpv 的網址：path 是 /Videos/… 這種（含 ? 後面的參數，不含金鑰）
pub fn url(server: &str, key: &str, path: &str) -> String {
    let Some(r) = relay() else {
        let sep = if path.contains('?') { '&' } else { '?' };
        return format!("{server}{path}{sep}api_key={key}");
    };
    let mut s = r.servers.lock().unwrap_or_else(|e| e.into_inner());
    let n = s.iter().position(|(u, k)| u == server && k == key).unwrap_or_else(|| {
        s.push((server.to_string(), key.to_string()));
        s.len() - 1
    });
    format!("http://127.0.0.1:{}/{}/{n}{path}", r.port, r.token)
}

/// 從轉送網址找回真正的 Jellyfin 網址與金鑰（給這個程式自己用 ureq 下載時）
pub fn resolve(url: &str) -> Option<(String, String)> {
    let r = relay()?;
    let rest = url.strip_prefix(&format!("http://127.0.0.1:{}/{}/", r.port, r.token))?;
    let (n, path) = rest.split_at(rest.find('/')?);
    let (server, key) = r.servers.lock().unwrap_or_else(|e| e.into_inner()).get(n.parse::<usize>().ok()?)?.clone();
    Some((format!("{server}{path}"), key))
}

fn reply(c: &mut TcpStream, status: &str) {
    let _ = write!(c, "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
}

fn serve(mut c: TcpStream) {
    let _ = c.set_read_timeout(Some(Duration::from_secs(30)));
    let Ok(read) = c.try_clone() else { return };
    let mut rd = BufReader::new(read.take(16 * 1024));
    let mut line = String::new();
    if rd.read_line(&mut line).is_err() {
        return;
    }
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let mut range = None;
    loop {
        let mut h = String::new();
        if rd.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            if k.trim().eq_ignore_ascii_case("range") {
                range = Some(v.trim().to_string());
            }
        }
    }
    let Some(r) = relay() else { return };
    let Some((to, key)) = resolve(&format!("http://127.0.0.1:{}{target}", r.port)) else { return reply(&mut c, "404 Not Found") };
    let head = match method {
        "GET" => false,
        "HEAD" => true,
        _ => return reply(&mut c, "405 Method Not Allowed"),
    };
    let mut req = crate::jellyfin::stream_agent().request(method, &to).set("X-Emby-Token", &key);
    if let Some(range) = &range {
        req = req.set("Range", range);
    }
    let res = match req.call() {
        Ok(res) | Err(ureq::Error::Status(_, res)) => res,
        Err(e) => {
            crate::info!("[Jellyfin] 轉送失敗：{}", crate::log::redact(&e.to_string()));
            return reply(&mut c, "502 Bad Gateway");
        }
    };
    let mut out = format!("HTTP/1.1 {} {}\r\n", res.status(), res.status_text());
    for h in ["Content-Type", "Content-Length", "Content-Range", "Accept-Ranges", "Last-Modified", "ETag"] {
        if let Some(v) = res.header(h) {
            out.push_str(&format!("{h}: {v}\r\n"));
        }
    }
    out.push_str("Connection: close\r\n\r\n");
    if c.write_all(out.as_bytes()).is_err() || head {
        return;
    }
    // 播放器關掉、跳到別的地方時連線會被切斷：寫失敗就結束
    let _ = std::io::copy(&mut res.into_reader(), &mut c);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_adds_the_key_and_passes_range() {
        // 假的 Jellyfin：回傳收到的金鑰標頭與 Range
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || {
            for mut c in l.incoming().flatten() {
                let mut buf = [0u8; 4096];
                let n = c.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                let pick = |name: &str| req.lines().find_map(|l| l.strip_prefix(name)).unwrap_or("").trim().to_string();
                let body = format!("{}|{}|{}", req.lines().next().unwrap_or(""), pick("x-emby-token:"), pick("range:"));
                let _ = write!(c, "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes 5-9/100\r\n\r\n{body}", body.len());
            }
        });
        let server = format!("http://{addr}");
        let u = url(&server, "secret", "/Videos/v1/stream?static=true");
        assert!(u.starts_with("http://127.0.0.1:") && !u.contains("secret"), "{u}");
        assert_eq!(resolve(&u), Some((format!("{server}/Videos/v1/stream?static=true"), "secret".into())));
        // 同一台伺服器：同一個編號
        assert_eq!(url(&server, "secret", "/x"), u.replace("/Videos/v1/stream?static=true", "/x"));
        let res = ureq::get(&u).set("Range", "bytes=5-9").call().unwrap();
        assert_eq!(res.status(), 206);
        assert_eq!(res.header("Content-Range"), Some("bytes 5-9/100"));
        assert_eq!(res.into_string().unwrap(), "get /videos/v1/stream?static=true http/1.1|secret|bytes=5-9");
        // 隨機碼不對：不轉送
        let bad = u.replace(&relay().unwrap().token, "0000");
        assert!(matches!(ureq::get(&bad).call(), Err(ureq::Error::Status(404, _))));
    }
}
