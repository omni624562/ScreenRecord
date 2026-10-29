//! 用 Windows 內建的 WinHTTP 連線（下載外部元件、檢查與下載新版本在公司網路上的備援）。
//!
//! 公司網路常用自動設定指令碼（PAC、WPAD）指定 Proxy，Proxy 又要用 Windows 帳號驗證
//! （NTLM、Kerberos）。瀏覽器都會自動處理，ureq 不會，所以 ureq 連不上時改用 WinHTTP：
//! Proxy 設定、帳號驗證與憑證都和系統一致。

// 其他平台只有測試會用到網址拆解與錯誤說明。
#![cfg_attr(not(windows), allow(dead_code))]

use crate::{tr, trf};

/// 拆開 `https://主機[:連接埠]/路徑`；只接受 HTTPS，不接受網址裡的帳號密碼
pub fn split_https(url: &str) -> Option<(String, u16, String)> {
    let rest = url.strip_prefix("https://")?;
    let (authority, path) = match rest.find(['/', '?', '#']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (authority, 443),
    };
    if host.is_empty() {
        return None;
    }
    // 片段（#…）不送給伺服器
    let path = path.split('#').next().unwrap_or_default();
    let path = if path.is_empty() || path.starts_with('?') { format!("/{path}") } else { path.to_string() };
    Some((host.to_string(), port, path))
}

/// 自訂的 Proxy 位址換成 WinHTTP 的寫法（主機:連接埠）；帶帳號密碼的不用 WinHTTP（ureq 已經會送）
pub fn named_proxy(p: &str) -> Option<String> {
    let p = p.trim();
    let p = p.strip_prefix("http://").unwrap_or(p);
    let p = p.trim_end_matches('/');
    (!p.is_empty() && !p.contains('@') && !p.contains("://")).then(|| p.to_string())
}

/// WinHTTP 錯誤碼的說明（不含網址）
pub fn describe(code: u32) -> String {
    let what = match code {
        12002 => tr!("連線逾時", "The connection timed out"),
        12007 => tr!("找不到伺服器（DNS）", "Couldn't find the server (DNS)"),
        12029 => tr!("無法連線到伺服器", "Couldn't connect to the server"),
        12030 | 12031 => tr!("連線被中斷", "The connection was interrupted"),
        12037 | 12038 | 12045 | 12169 | 12175 => tr!("憑證有問題（公司網路的憑證可能沒有安裝到 Windows）", "Certificate problem (your company's certificate may not be installed in Windows)"),
        12166 => tr!("Proxy 的自動設定指令碼有錯誤", "The proxy's automatic configuration script has an error"),
        12167 => tr!("無法下載 Proxy 的自動設定指令碼", "Couldn't download the proxy's automatic configuration script"),
        12180 => tr!("找不到 Proxy 的自動設定", "Couldn't find the automatic proxy configuration"),
        _ => "",
    };
    if what.is_empty() {
        trf!("WinHTTP 錯誤 {code}", "WinHTTP error {code}")
    } else {
        trf!("{what}（WinHTTP {code}）", "{what} (WinHTTP {code})")
    }
}

#[cfg(windows)]
pub use imp::{get, Response};

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;
    use std::io::Read;
    use std::ptr::null_mut;
    use std::time::Duration;

    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::GetLastError;
    use windows::Win32::Networking::WinHttp::*;

    use super::{describe, split_https};
    use crate::tr;

    /// windows 的錯誤（HRESULT_FROM_WIN32）換回 WinHTTP 的錯誤碼
    fn code_of(e: &windows::core::Error) -> u32 {
        (e.code().0 as u32) & 0xFFFF
    }

    fn explain(e: windows::core::Error) -> String {
        describe(code_of(&e))
    }

    fn last_error() -> String {
        describe(unsafe { GetLastError() }.0)
    }

    /// WinHTTP 的控制代碼，離開時自動關閉
    struct Handle(*mut c_void);

    // 同步模式的 WinHTTP 控制代碼可以在別的執行緒使用（下載在背景執行緒讀）
    unsafe impl Send for Handle {}

    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                let _ = unsafe { WinHttpCloseHandle(self.0) };
            }
        }
    }

    fn handle(h: *mut c_void) -> Result<Handle, String> {
        if h.is_null() {
            Err(last_error())
        } else {
            Ok(Handle(h))
        }
    }

    /// 回應：狀態碼、內容長度與內容（`Read`）。欄位順序就是關閉順序：先關請求，再關連線與工作階段
    pub struct Response {
        pub status: u16,
        pub len: Option<u64>,
        request: Handle,
        _connect: Handle,
        _session: Handle,
    }

    impl Read for Response {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let mut n = 0u32;
            let len = buf.len().min(u32::MAX as usize) as u32;
            unsafe { WinHttpReadData(self.request.0, buf.as_mut_ptr().cast(), len, &mut n) }.map_err(|e| std::io::Error::from_raw_os_error(code_of(&e) as i32))?;
            Ok(n as usize)
        }
    }

    fn query_u32(request: &Handle, what: u32) -> Option<u32> {
        let mut v = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        unsafe { WinHttpQueryHeaders(request.0, what | WINHTTP_QUERY_FLAG_NUMBER, PCWSTR::null(), Some((&mut v as *mut u32).cast()), &mut size, null_mut()) }.ok()?;
        Some(v)
    }

    fn query_u64(request: &Handle, what: u32) -> Option<u64> {
        let mut v = 0u64;
        let mut size = std::mem::size_of::<u64>() as u32;
        unsafe { WinHttpQueryHeaders(request.0, what | WINHTTP_QUERY_FLAG_NUMBER64, PCWSTR::null(), Some((&mut v as *mut u64).cast()), &mut size, null_mut()) }.ok()?;
        Some(v)
    }

    /// 丟掉這次回應剩下的內容（重送請求前要先讀完）
    fn drain(request: &Handle) {
        let mut buf = [0u8; 8192];
        loop {
            let mut n = 0u32;
            if unsafe { WinHttpReadData(request.0, buf.as_mut_ptr().cast(), buf.len() as u32, &mut n) }.is_err() || n == 0 {
                break;
            }
        }
    }

    /// 以 GET 連到 `url`（只接受 HTTPS），任何狀態碼都回傳 Ok（由呼叫端判斷）。
    /// `proxy` 是 None 時用系統的 Proxy 設定（含自動設定指令碼 PAC、WPAD），否則用指定的那一個。
    /// WinHTTP 會自己處理轉址（不會從 HTTPS 轉到 HTTP）；Proxy 要求驗證（407）時用目前登入的 Windows 帳號（NTLM、Kerberos）
    pub fn get(url: &str, headers: &[(&str, &str)], proxy: Option<&str>, timeout: Duration, user_agent: &str) -> Result<Response, String> {
        let (host, port, path) = split_https(url).ok_or_else(|| tr!("只能用 HTTPS 下載", "Only HTTPS downloads are supported").to_string())?;
        let agent = HSTRING::from(user_agent);
        let session = match proxy {
            Some(p) => handle(unsafe { WinHttpOpen(&agent, WINHTTP_ACCESS_TYPE_NAMED_PROXY, &HSTRING::from(p), &HSTRING::from("<local>"), 0) })?,
            // 自動 Proxy（Windows 8.1 以後）：系統與使用者的 Proxy 設定、自動設定指令碼、WPAD；舊版 Windows 改用預設
            None => handle(unsafe { WinHttpOpen(&agent, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0) })
                .or_else(|_| handle(unsafe { WinHttpOpen(&agent, WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, PCWSTR::null(), PCWSTR::null(), 0) }))?,
        };
        let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        unsafe { WinHttpSetTimeouts(session.0, ms, ms, ms, ms) }.map_err(explain)?;
        let connect = handle(unsafe { WinHttpConnect(session.0, &HSTRING::from(host), port, 0) })?;
        let request = handle(unsafe { WinHttpOpenRequest(connect.0, &HSTRING::from("GET"), &HSTRING::from(path), PCWSTR::null(), PCWSTR::null(), null_mut(), WINHTTP_FLAG_SECURE) })?;
        if !headers.is_empty() {
            let text: String = headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
            let wide: Vec<u16> = text.encode_utf16().collect();
            unsafe { WinHttpAddRequestHeaders(request.0, &wide, WINHTTP_ADDREQ_FLAG_ADD | WINHTTP_ADDREQ_FLAG_REPLACE) }.map_err(explain)?;
        }
        // 允許自動送出 Windows 帳號（只送給 Proxy 這類內部網路的對象；GitHub 等外部網站不會要求）
        let policy = WINHTTP_AUTOLOGON_SECURITY_LEVEL_LOW.to_ne_bytes();
        let _ = unsafe { WinHttpSetOption(Some(request.0), WINHTTP_OPTION_AUTOLOGON_POLICY, Some(&policy)) };
        for _ in 0..3 {
            unsafe { WinHttpSendRequest(request.0, None, None, 0, 0, 0) }.map_err(explain)?;
            unsafe { WinHttpReceiveResponse(request.0, null_mut()) }.map_err(explain)?;
            let status = query_u32(&request, WINHTTP_QUERY_STATUS_CODE).ok_or_else(last_error)?;
            if status != 407 {
                let len = query_u64(&request, WINHTTP_QUERY_CONTENT_LENGTH);
                return Ok(Response { status: status.min(u16::MAX as u32) as u16, len, request, _connect: connect, _session: session });
            }
            let (mut supported, mut first, mut target) = (0u32, 0u32, 0u32);
            unsafe { WinHttpQueryAuthSchemes(request.0, &mut supported, &mut first, &mut target) }.map_err(explain)?;
            let scheme = [WINHTTP_AUTH_SCHEME_NEGOTIATE, WINHTTP_AUTH_SCHEME_NTLM].into_iter().find(|s| supported & s.0 != 0).ok_or_else(|| {
                tr!("公司的 Proxy 要輸入帳號密碼，請在「自訂」填 http://帳號:密碼@Proxy位址", "Your company's proxy needs a user name and password. Enter http://user:password@proxy under Custom")
                    .to_string()
            })?;
            unsafe { WinHttpSetCredentials(request.0, WINHTTP_AUTH_TARGET_PROXY, scheme.0, PCWSTR::null(), PCWSTR::null(), null_mut()) }.map_err(explain)?;
            drain(&request);
        }
        Err(tr!("公司的 Proxy 拒絕了這台電腦的 Windows 帳號（HTTP 407）", "Your company's proxy rejected this computer's Windows account (HTTP 407)").to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_https_urls() {
        assert_eq!(split_https("https://github.com/o/r/releases/download/v1.0.0/app.exe"), Some(("github.com".into(), 443, "/o/r/releases/download/v1.0.0/app.exe".into())));
        assert_eq!(split_https("https://example.com:8443?x=1"), Some(("example.com".into(), 8443, "/?x=1".into())));
        assert_eq!(split_https("https://example.com"), Some(("example.com".into(), 443, "/".into())));
        assert_eq!(split_https("https://example.com/a#top"), Some(("example.com".into(), 443, "/a".into())));
        assert_eq!(split_https("http://example.com/a"), None);
        assert_eq!(split_https("https://user@example.com/a"), None);
        assert_eq!(split_https("https:///a"), None);
        assert_eq!(split_https("https://:443/a"), None);
        assert_eq!(split_https("https://example.com:x/a"), None);
    }

    #[test]
    fn named_proxies() {
        assert_eq!(named_proxy(" proxy.example.com:8080 ").as_deref(), Some("proxy.example.com:8080"));
        assert_eq!(named_proxy("http://proxy.example.com:8080/").as_deref(), Some("proxy.example.com:8080"));
        assert_eq!(named_proxy("http://user:pass@proxy.example.com:8080"), None);
        assert_eq!(named_proxy("socks5://proxy.example.com:1080"), None);
        assert_eq!(named_proxy(""), None);
    }

    #[test]
    fn explains_common_errors() {
        assert!(describe(12007).contains("DNS"));
        assert!(describe(12167).contains("自動設定"));
        assert_eq!(describe(99999), "WinHTTP 錯誤 99999");
    }
}
