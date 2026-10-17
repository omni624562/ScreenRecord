//! Jellyfin：錄完自動處理複製到媒體庫資料夾後，通知 Jellyfin 重新掃描，新影片馬上出現（不用等定時掃描）。
//! 自家伺服器多半在區域網路：直接連線，不經過 Proxy。

use crate::tr;
use crate::trf;
use serde_json::Value;
use std::time::Duration;

/// ui 設定裡的欄位名稱（是個人的連線資料，不放進設定組合）
pub const URL_KEY: &str = "jellyfinUrl";
pub const API_KEY: &str = "jellyfinKey";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jellyfin {
    /// 伺服器網址，例如 http://192.168.1.10:8096（結尾沒有 /）
    pub url: String,
    pub key: String,
}

impl Jellyfin {
    /// 網址和 API 金鑰都有填才算設定好
    pub fn from_ui(ui: Option<&Value>) -> Option<Jellyfin> {
        let o = ui?.as_object()?;
        let get = |k: &str| o.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
        Jellyfin::new(&get(URL_KEY), &get(API_KEY))
    }

    pub fn new(url: &str, key: &str) -> Option<Jellyfin> {
        let url = normalize_url(url)?;
        let key = key.trim();
        (!key.is_empty()).then(|| Jellyfin { url, key: key.to_string() })
    }

    /// 測試連線：成功回傳「伺服器名稱（版本）」
    pub fn test(&self) -> Result<String, String> {
        let r = self.call(agent().get(&format!("{}/System/Info", self.url)))?;
        let info: Value = r
            .into_string()
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .ok_or_else(|| tr!("回應看不懂，這個網址可能不是 Jellyfin", "Couldn't understand the reply. This address may not be Jellyfin").to_string())?;
        let name = info.get("ServerName").and_then(Value::as_str).unwrap_or("Jellyfin");
        Ok(match info.get("Version").and_then(Value::as_str) {
            Some(v) => format!("{name}（{v}）"),
            None => name.to_string(),
        })
    }

    /// 請 Jellyfin 重新掃描所有媒體庫（掃描在伺服器上跑，這裡不等它掃完）
    pub fn refresh(&self) -> Result<(), String> {
        self.call(agent().post(&format!("{}/Library/Refresh", self.url))).map(|_| ())
    }

    /// 帶上 API 金鑰送出；失敗時回傳給使用者看的原因
    fn call(&self, req: ureq::Request) -> Result<ureq::Response, String> {
        req.set("Authorization", &format!("MediaBrowser Token=\"{}\"", self.key)).set("X-Emby-Token", &self.key).call().map_err(|e| describe(&e))
    }
}

/// 使用者輸入的網址：補上 http://、去掉結尾的 /；空的或看不懂的回傳 None
pub fn normalize_url(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() || t.contains(char::is_whitespace) {
        return None;
    }
    let lower = t.to_ascii_lowercase();
    let (scheme, rest) = ["http://", "https://"].iter().find(|p| lower.starts_with(**p)).map_or(("http://", t), |p| (&t[..p.len()], &t[p.len()..]));
    let rest = rest.trim_end_matches('/');
    (!rest.is_empty() && !rest.starts_with('/')).then(|| format!("{scheme}{rest}"))
}

/// 區域網路的伺服器：直接連線（不經過 Proxy），等太久就放棄
fn agent() -> ureq::Agent {
    let b = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(8)).timeout_read(Duration::from_secs(15));
    #[cfg(windows)]
    let b = match native_tls::TlsConnector::new() {
        Ok(c) => b.tls_connector(std::sync::Arc::new(c)),
        Err(_) => b,
    };
    b.build()
}

fn describe(e: &ureq::Error) -> String {
    match e {
        ureq::Error::Status(401 | 403, _) => tr!("API 金鑰不對（Jellyfin 控制台 → API 金鑰）", "The API key is wrong (Jellyfin Dashboard → API Keys)").into(),
        ureq::Error::Status(404, _) => tr!("網址不對，找不到 Jellyfin（預設是 http://伺服器:8096）", "Wrong address; Jellyfin wasn't found there (usually http://server:8096)").into(),
        ureq::Error::Status(code, _) => trf!("Jellyfin 回應錯誤（HTTP {code}）", "Jellyfin returned an error (HTTP {code})"),
        ureq::Error::Transport(_) => {
            tr!("連不到 Jellyfin，請確認網址、伺服器有開著、和這台電腦在同一個網路", "Couldn't reach Jellyfin. Check the address, that the server is on, and that it's on the same network as this PC")
                .into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    #[test]
    fn urls_are_normalized() {
        assert_eq!(normalize_url(" 192.168.1.10:8096/ ").as_deref(), Some("http://192.168.1.10:8096"));
        assert_eq!(normalize_url("https://jf.example.com/jellyfin/").as_deref(), Some("https://jf.example.com/jellyfin"));
        assert_eq!(normalize_url("HTTP://nas:8096").as_deref(), Some("HTTP://nas:8096"));
        assert_eq!(normalize_url(""), None);
        assert_eq!(normalize_url("http://"), None);
        assert_eq!(normalize_url("my server"), None);
        assert!(Jellyfin::new("nas:8096", "  ").is_none());
        let ui = serde_json::json!({ URL_KEY: "nas:8096", API_KEY: " abc " });
        assert_eq!(Jellyfin::from_ui(Some(&ui)), Some(Jellyfin { url: "http://nas:8096".into(), key: "abc".into() }));
        assert!(Jellyfin::from_ui(Some(&serde_json::json!({ URL_KEY: "nas:8096" }))).is_none());
    }

    /// 假的 Jellyfin：回應一次，回傳收到的請求行與標頭
    fn serve_once(status: &str, body: &'static str) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let status = status.to_string();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut lines = vec![];
            let mut r = BufReader::new(s.try_clone().unwrap());
            loop {
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                if line.trim().is_empty() {
                    break;
                }
                lines.push(line.trim().to_string());
            }
            write!(s, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            lines
        });
        (url, h)
    }

    #[test]
    fn refresh_posts_with_the_api_key() {
        let (url, h) = serve_once("204 No Content", "");
        Jellyfin::new(&url, "k123").unwrap().refresh().unwrap();
        let req = h.join().unwrap();
        assert_eq!(req[0], "POST /Library/Refresh HTTP/1.1");
        assert!(req.iter().any(|l| l == "X-Emby-Token: k123"), "{req:?}");
        assert!(req.iter().any(|l| l == "Authorization: MediaBrowser Token=\"k123\""), "{req:?}");
    }

    #[test]
    fn test_reports_server_name_or_why_not() {
        let (url, h) = serve_once("200 OK", r#"{"ServerName":"家裡的NAS","Version":"10.10.7"}"#);
        assert_eq!(Jellyfin::new(&url, "k").unwrap().test().unwrap(), "家裡的NAS（10.10.7）");
        assert!(h.join().unwrap()[0].starts_with("GET /System/Info "));
        let (url, h) = serve_once("401 Unauthorized", "");
        assert!(Jellyfin::new(&url, "bad").unwrap().test().unwrap_err().contains("API"));
        h.join().unwrap();
        // 沒有開的連接埠
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        assert!(Jellyfin::new(&format!("127.0.0.1:{port}"), "k").unwrap().refresh().is_err());
    }
}
