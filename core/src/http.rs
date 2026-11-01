//! HTTPS 連線設定：Windows 用系統內建的 SChannel（使用系統憑證、遵循企業環境的設定），其他平台用 rustls。
//! ureq 連不上時，Windows 上改用系統內建的 WinHTTP 再試一次（`crate::winhttp`：公司網路的自動設定指令碼 PAC、WPAD 與 Windows 帳號驗證）。
//! 連線失敗的說明一律經過 describe：ureq 錯誤本身的文字開頭就是完整網址，不能直接顯示或寫進記錄檔。

use crate::{tr, trf};
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Proxy 伺服器的設定（設定 → 外部元件）
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ProxyMode {
    /// 跟著系統：環境變數 HTTPS_PROXY 等，或 Windows 的 Proxy 設定（手動設定的那一個）
    #[default]
    Auto,
    /// 不用 Proxy
    Off,
    /// 自己填的，例如 proxy.company.com:8080
    Custom(String),
}

static PROXY: std::sync::RwLock<ProxyMode> = std::sync::RwLock::new(ProxyMode::Auto);

/// 從操作視窗的設定讀出 Proxy（"proxyMode"：auto / off / custom，"proxy"：位址）
pub fn proxy_from_ui(ui: Option<&serde_json::Value>) -> ProxyMode {
    let get = |k: &str| ui.and_then(|u| u.get(k)).and_then(|v| v.as_str()).map(str::trim).unwrap_or_default();
    match get("proxyMode") {
        "off" => ProxyMode::Off,
        "custom" if !get("proxy").is_empty() => ProxyMode::Custom(get("proxy").to_string()),
        _ => ProxyMode::Auto,
    }
}

/// 套用設定（啟動時、存設定時）
pub fn configure(ui: Option<&serde_json::Value>) {
    let m = proxy_from_ui(ui);
    let mut g = PROXY.write().unwrap_or_else(|e| e.into_inner());
    if *g != m {
        crate::info!("[網路] Proxy：{}", describe_mode(&m));
        *g = m;
    }
}

fn describe_mode(m: &ProxyMode) -> String {
    match m {
        ProxyMode::Auto => match system_proxy() {
            Some(p) => format!("跟著系統（{}）", hide_password(&p)),
            None => "跟著系統（沒有設定）".into(),
        },
        ProxyMode::Off => "不使用".into(),
        ProxyMode::Custom(p) => hide_password(p),
    }
}

/// 要顯示、寫進記錄檔的 Proxy 位址：遮掉密碼（http://帳號:密碼@主機 → http://帳號:…@主機）
pub fn hide_password(url: &str) -> String {
    let Some(at) = url.rfind('@') else { return url.to_string() };
    let start = url.find("://").map_or(0, |i| i + 3);
    if at < start {
        return url.to_string();
    }
    let user = &url[start..at];
    let name = user.split_once(':').map_or(user, |(n, _)| n);
    format!("{}{name}:…{}", &url[..start], &url[at..])
}

/// 系統的 Proxy：環境變數，或 Windows「設定 → 網路和網際網路 → Proxy」手動設定的伺服器
/// （自動設定指令碼 PAC、WPAD 這裡讀不到，連不上時由 WinHTTP 處理）
pub fn system_proxy() -> Option<String> {
    for k in ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy", "HTTP_PROXY", "http_proxy"] {
        if let Some(v) = std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) {
            return Some(v);
        }
    }
    windows_proxy()
}

/// Windows 的 ProxyServer 可能是 "host:port" 或 "http=a:80;https=b:443"：優先用 https 那一個
pub fn pick_proxy_server(v: &str) -> Option<String> {
    let v = v.trim();
    if v.is_empty() {
        return None;
    }
    if !v.contains('=') {
        return Some(v.to_string());
    }
    let parts: Vec<(&str, &str)> = v.split(';').filter_map(|p| p.split_once('=')).map(|(k, v)| (k.trim(), v.trim())).filter(|(_, v)| !v.is_empty()).collect();
    ["https", "http"].iter().find_map(|want| parts.iter().find(|(k, _)| k.eq_ignore_ascii_case(want)).map(|(_, v)| v.to_string()))
}

#[cfg(windows)]
fn windows_proxy() -> Option<String> {
    use windows::core::w;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RRF_RT_REG_SZ};
    let key = w!(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings");
    unsafe {
        let mut on: u32 = 0;
        let mut n = 4u32;
        if RegGetValueW(HKEY_CURRENT_USER, key, w!("ProxyEnable"), RRF_RT_REG_DWORD, None, Some((&mut on as *mut u32).cast()), Some(&mut n)).is_err() || on == 0 {
            return None;
        }
        let mut buf = vec![0u16; 1024];
        let mut bytes = (buf.len() * 2) as u32;
        if RegGetValueW(HKEY_CURRENT_USER, key, w!("ProxyServer"), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut bytes)).is_err() {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        pick_proxy_server(&String::from_utf16_lossy(&buf[..len]))
    }
}

#[cfg(not(windows))]
fn windows_proxy() -> Option<String> {
    None
}

/// 目前要用的 Proxy（None：直接連線）
pub fn current_proxy() -> Option<String> {
    match &*PROXY.read().unwrap_or_else(|e| e.into_inner()) {
        // 測試連到本機的假伺服器，不經過開發環境的 Proxy
        ProxyMode::Auto if cfg!(test) => None,
        ProxyMode::Auto => system_proxy(),
        ProxyMode::Off => None,
        ProxyMode::Custom(p) => Some(p.clone()),
    }
}

/// 試著連線下載來源（設定 → 外部元件的「測試連線」）：成功回傳 Ok，失敗回傳原因
pub fn test_connection() -> Result<(), String> {
    get("https://github.com/", &[], Duration::from_secs(15), Duration::from_secs(15)).map(|_| ())
}

/// 回應：HTTP 狀態碼、內容長度（可能未知）與內容
pub struct Response {
    pub status: u16,
    pub len: Option<u64>,
    pub body: Box<dyn Read + Send>,
}

/// WinHTTP 連上過（ureq 連不上）之後，這次執行都先用 WinHTTP，不必每次先等 ureq 逾時
static PREFER_WINHTTP: AtomicBool = AtomicBool::new(false);

fn user_agent() -> String {
    format!("ScreenRecorder/{}", crate::version::APP_VERSION)
}

/// GET `url`，任何狀態碼都回傳 Ok（由呼叫端判斷），失敗的說明不含網址。
/// `connect`：等待建立連線；`read`：每次等待回應、讀取內容（沒有整體的時間限制，大檔案也能下載完）。
/// Windows 上 ureq 連不上時（公司網路的 Proxy 用自動設定指令碼 PAC、WPAD，或要用 Windows 帳號驗證）
/// 改用系統內建的 WinHTTP 再試一次；Proxy 選「不使用」時不試
pub fn get(url: &str, headers: &[(&str, &str)], connect: Duration, read: Duration) -> Result<Response, String> {
    let mut alt = winhttp_proxy();
    if let Some(proxy) = &alt {
        if PREFER_WINHTTP.load(Ordering::Relaxed) {
            match winhttp_get(url, headers, proxy.as_deref(), read) {
                Ok(r) => return Ok(r),
                Err(e) => crate::warn!("[網路] WinHTTP 連線失敗，改用一般連線：{e}"),
            }
            // 已經試過，一般連線也失敗時不再試一次
            alt = None;
        }
    }
    let first = match ureq_get(url, headers, connect, read) {
        Ok(r) => return Ok(r),
        Err(e) => e,
    };
    let Some(proxy) = alt else { return Err(first) };
    match winhttp_get(url, headers, proxy.as_deref(), read) {
        Ok(r) => {
            if !PREFER_WINHTTP.swap(true, Ordering::Relaxed) {
                crate::info!("[網路] 一般連線失敗（{first}），改用 Windows 的連線設定（WinHTTP）連上");
            }
            Ok(r)
        }
        Err(e) => Err(trf!("{e}（另一種連線方式：{first}）", "{e} (other connection method: {first})")),
    }
}

/// 下載 `url`：只接受 2xx，回傳（內容長度, 內容）
pub fn download(url: &str, connect: Duration, read: Duration) -> Result<(Option<u64>, Box<dyn Read + Send>), String> {
    let r = get(url, &[], connect, read)?;
    if !(200..300).contains(&r.status) {
        return Err(status_error(r.status));
    }
    Ok((r.len, r.body))
}

/// ureq 連線；伺服器回應錯誤（4xx、5xx）也算連上，只有連不上時回傳 Err（說明）
fn ureq_get(url: &str, headers: &[(&str, &str)], connect: Duration, read: Duration) -> Result<Response, String> {
    let mut req = agent().timeout_connect(connect).timeout_read(read).redirects(8).build().get(url).set("User-Agent", &user_agent());
    for (k, v) in headers {
        req = req.set(k, v);
    }
    let r = match req.call() {
        Ok(r) | Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return Err(describe(&e)),
    };
    let len = r.header("Content-Length").and_then(|v| v.parse().ok());
    Ok(Response { status: r.status(), len, body: Box::new(r.into_reader()) })
}

/// 要不要改用 WinHTTP 再試、用哪個 Proxy：None 不試；Some(None) 跟著系統（含自動設定指令碼）；Some(Some(位址)) 自訂的 Proxy
fn winhttp_proxy() -> Option<Option<String>> {
    if !cfg!(windows) || cfg!(test) {
        return None;
    }
    match &*PROXY.read().unwrap_or_else(|e| e.into_inner()) {
        ProxyMode::Auto => Some(None),
        ProxyMode::Off => None,
        ProxyMode::Custom(p) => crate::winhttp::named_proxy(p).map(Some),
    }
}

#[cfg(windows)]
fn winhttp_get(url: &str, headers: &[(&str, &str)], proxy: Option<&str>, read: Duration) -> Result<Response, String> {
    let r = crate::winhttp::get(url, headers, proxy, read, &user_agent())?;
    Ok(Response { status: r.status, len: r.len, body: Box::new(r) })
}

#[cfg(not(windows))]
fn winhttp_get(_url: &str, _headers: &[(&str, &str)], _proxy: Option<&str>, _read: Duration) -> Result<Response, String> {
    Err(tr!("只有 Windows 有 WinHTTP", "WinHTTP is only available on Windows").into())
}

pub fn agent() -> ureq::AgentBuilder {
    let mut b = ureq::AgentBuilder::new();
    if let Some(p) = current_proxy() {
        match ureq::Proxy::new(&p) {
            Ok(proxy) => b = b.proxy(proxy),
            Err(_) => crate::warn!("[網路] Proxy 位址看不懂，改成直接連線"),
        }
    }
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
        ureq::Error::Status(code, _) => return status_error(*code),
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

/// 伺服器回應錯誤的說明
pub fn status_error(code: u16) -> String {
    trf!("伺服器回應錯誤（HTTP {code}）", "The server returned an error (HTTP {code})")
}

/// 下載途中讀取失敗的說明（同樣不含網址）
pub fn describe_io(e: &io::Error) -> String {
    // WinHTTP 讀取失敗（錯誤碼 12001～12999）
    if let Some(code) = e.raw_os_error().filter(|c| (12001..13000).contains(c)) {
        return crate::winhttp::describe(code as u32);
    }
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

    #[test]
    fn proxy_passwords_are_hidden() {
        assert_eq!(hide_password("http://me:secret@proxy:8080"), "http://me:…@proxy:8080");
        assert_eq!(hide_password("me:p@ss@proxy:8080"), "me:…@proxy:8080");
        assert_eq!(hide_password("proxy.corp:8080"), "proxy.corp:8080");
    }

    #[test]
    fn proxy_settings() {
        assert_eq!(pick_proxy_server("proxy.corp:8080").as_deref(), Some("proxy.corp:8080"));
        assert_eq!(pick_proxy_server("http=a:80;https=b:443;ftp=c:21").as_deref(), Some("b:443"));
        assert_eq!(pick_proxy_server("http=a:80").as_deref(), Some("a:80"));
        assert_eq!(pick_proxy_server("ftp=c:21"), None);
        assert_eq!(pick_proxy_server(" "), None);
        let ui = serde_json::json!({ "proxyMode": "custom", "proxy": " proxy.corp:8080 " });
        assert_eq!(proxy_from_ui(Some(&ui)), ProxyMode::Custom("proxy.corp:8080".into()));
        assert_eq!(proxy_from_ui(Some(&serde_json::json!({ "proxyMode": "custom", "proxy": "" }))), ProxyMode::Auto);
        assert_eq!(proxy_from_ui(Some(&serde_json::json!({ "proxyMode": "off" }))), ProxyMode::Off);
        assert_eq!(proxy_from_ui(None), ProxyMode::Auto);
    }
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

    /// 本機的假伺服器：每個連線依序回一個回應
    fn serve(replies: Vec<&'static [u8]>) -> u16 {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::Write;
            for reply in replies {
                let (mut s, _) = server.accept().unwrap();
                let _ = s.read(&mut [0u8; 4096]);
                let _ = s.write_all(reply);
            }
        });
        port
    }

    #[test]
    fn get_returns_status_length_and_body() {
        let port = serve(vec![b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello", b"HTTP/1.1 404 Not Found\r\nContent-Length: 7\r\n\r\nmissing"]);
        let url = format!("http://127.0.0.1:{port}/example");
        let wait = Duration::from_secs(5);
        let mut r = get(&url, &[("Accept", "text/plain")], wait, wait).unwrap();
        let mut body = String::new();
        r.body.read_to_string(&mut body).unwrap();
        assert_eq!((r.status, r.len, body.as_str()), (200, Some(5), "hello"));
        // 伺服器回應錯誤也回傳內容（GitHub 用 403 說明查詢次數用完）；download 只接受 2xx
        let e = download(&url, wait, wait).err().unwrap();
        assert_eq!(e, "伺服器回應錯誤（HTTP 404）");
    }

    #[test]
    fn unreachable_get_has_no_url() {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let e = get(&format!("http://127.0.0.1:{port}/example"), &[], Duration::from_secs(5), Duration::from_secs(5)).err().unwrap();
        assert!(e.starts_with("無法連線"), "{e}");
        assert_no_url(&e);
    }

    #[test]
    fn winhttp_read_errors_are_explained() {
        assert_eq!(describe_io(&io::Error::from_raw_os_error(12002)), "連線逾時（WinHTTP 12002）");
    }
}
