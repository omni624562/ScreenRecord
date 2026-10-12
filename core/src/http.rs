//! HTTPS 連線設定：Windows 用系統內建的 SChannel（使用系統憑證、遵循企業環境的設定），其他平台用 rustls。
//! 連線失敗的說明一律經過 describe：ureq 錯誤本身的文字開頭就是完整網址，不能直接顯示或寫進記錄檔。

use crate::{tr, trf};
use std::io;

pub fn agent() -> ureq::AgentBuilder {
    let b = ureq::AgentBuilder::new();
    #[cfg(windows)]
    let b = match native_tls::TlsConnector::new() {
        Ok(c) => b.tls_connector(std::sync::Arc::new(c)),
        Err(_) => b,
    };
    b
}

/// 連線失敗的說明（顯示在介面、寫進記錄檔）：只說原因與錯誤代碼，不含網址或主機名稱
pub fn describe(e: &ureq::Error) -> String {
    let t = match e {
        ureq::Error::Status(code, _) => return trf!("伺服器回應錯誤（HTTP {code}）", "The server returned an error (HTTP {code})"),
        ureq::Error::Transport(t) => t,
    };
    let io = io_source(t);
    let tls = t.message().is_some_and(|m| m.to_ascii_lowercase().contains("tls"));
    let text = match t.kind() {
        _ if io.is_some_and(|e| e.kind() == io::ErrorKind::TimedOut) => timed_out(),
        ureq::ErrorKind::Dns => tr!("找不到伺服器，請確認已連上網路", "Couldn't find the server. Check that you're connected to the internet"),
        ureq::ErrorKind::ConnectionFailed if tls => tr!("無法建立安全連線，可能被防火牆或防毒軟體攔截", "Couldn't establish a secure connection. A firewall or antivirus may be blocking it"),
        ureq::ErrorKind::ConnectionFailed => tr!("無法連線，請確認已連上網路，或防火牆沒有擋住本程式", "Couldn't connect. Check your internet connection, or whether a firewall is blocking this app"),
        ureq::ErrorKind::InvalidProxyUrl | ureq::ErrorKind::ProxyConnect | ureq::ErrorKind::ProxyUnauthorized => {
            tr!("無法透過 Proxy 伺服器連線，請確認 Proxy 設定", "Couldn't connect through the proxy server. Check your proxy settings")
        }
        ureq::ErrorKind::Io => interrupted(),
        _ => tr!("連線失敗", "Connection failed"),
    };
    with_code(text, io)
}

/// 下載途中讀取失敗的說明（同樣不含網址）
pub fn describe_io(e: &io::Error) -> String {
    with_code(if e.kind() == io::ErrorKind::TimedOut { timed_out() } else { interrupted() }, Some(e))
}

fn timed_out() -> &'static str {
    tr!("連線逾時，請確認已連上網路，或防火牆沒有擋住本程式", "The connection timed out. Check your internet connection, or whether a firewall is blocking this app")
}

fn interrupted() -> &'static str {
    tr!("連線中斷，請確認網路連線是否穩定", "The connection was interrupted. Check that your internet connection is stable")
}

/// 附上系統的錯誤代碼（方便查詢原因）；負數（SChannel 的 HRESULT）以 0x 十六進位表示
fn with_code(text: &str, io: Option<&io::Error>) -> String {
    match io.and_then(io::Error::raw_os_error) {
        Some(c) if c < 0 => trf!("{text}（錯誤代碼 0x{:08X}）", "{text} (error code 0x{:08X})", c as u32),
        Some(c) => trf!("{text}（錯誤代碼 {c}）", "{text} (error code {c})"),
        None => text.to_string(),
    }
}

/// 錯誤來源中的系統 I/O 錯誤（連線被拒、逾時、憑證不受信任等）
fn io_source(t: &ureq::Transport) -> Option<&io::Error> {
    let mut cur = std::error::Error::source(t);
    while let Some(e) = cur {
        if let Some(io) = e.downcast_ref::<io::Error>() {
            return Some(io);
        }
        cur = e.source();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::time::Duration;

    /// 每一種失敗的說明都不能帶出網址或主機名稱
    fn assert_no_url(msg: &str) {
        for bad in ["http", "://", "127.0.0.1", "localhost", "invalid", "github", "example"] {
            assert!(!msg.to_lowercase().contains(bad), "說明含有「{bad}」：{msg}");
        }
    }

    #[test]
    fn connection_refused_has_no_url() {
        // 先佔用再釋放一個連接埠：連過去會被拒絕
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let e = agent().timeout(Duration::from_secs(5)).build().get(&format!("http://127.0.0.1:{port}/repos/example/releases")).call().unwrap_err();
        assert!(e.to_string().contains("127.0.0.1"), "ureq 原本的文字含網址：{e}");
        let msg = describe(&e);
        assert!(msg.starts_with("無法連線"), "{msg}");
        assert!(msg.contains("錯誤代碼"), "{msg}");
        assert_no_url(&msg);
    }

    #[test]
    fn dns_failure_has_no_url() {
        let e = agent().timeout(Duration::from_secs(5)).build().get("https://no-such-host.invalid/example").call().unwrap_err();
        assert_no_url(&describe(&e));
    }

    #[test]
    fn status_and_read_errors_have_no_url() {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut s, _) = server.accept().unwrap();
            let _ = s.read(&mut [0u8; 1024]);
            let _ = s.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n");
        });
        let e = agent().build().get(&format!("http://127.0.0.1:{port}/example")).call().unwrap_err();
        assert!(e.to_string().contains("127.0.0.1"), "ureq 原本的文字含網址：{e}");
        assert_eq!(describe(&e), "伺服器回應錯誤（HTTP 503）");

        assert!(describe_io(&io::Error::new(io::ErrorKind::TimedOut, "timed out reading https://example/x")).starts_with("連線逾時"));
        let reset = describe_io(&io::Error::from_raw_os_error(10054));
        assert!(reset.starts_with("連線中斷") && reset.ends_with("（錯誤代碼 10054）"), "{reset}");
        assert_no_url(&reset);
        assert!(with_code("x", Some(&io::Error::from_raw_os_error(-2146893019))).ends_with("0x80090325）"));
    }
}
