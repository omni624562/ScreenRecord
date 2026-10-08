//! HTTPS 連線設定：Windows 用系統內建的 SChannel（使用系統憑證、遵循企業環境的設定），其他平台用 rustls。

pub fn agent() -> ureq::AgentBuilder {
    let b = ureq::AgentBuilder::new();
    #[cfg(windows)]
    let b = match native_tls::TlsConnector::new() {
        Ok(c) => b.tls_connector(std::sync::Arc::new(c)),
        Err(_) => b,
    };
    b
}
