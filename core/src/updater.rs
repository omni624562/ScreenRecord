//! 檢查新版本：查詢 GitHub Releases 的最新版本，比目前新就提示（不會自動下載或安裝）。
//! 只送出一個不帶任何個人資料的 GET 要求；網路不通或被限流時由呼叫端略過。

use crate::types::UpdateInfo;
use serde::Deserialize;
use std::time::Duration;

pub const RELEASES_API: &str = "https://api.github.com/repos/omni624562/ScreenRecord/releases/latest";
const PROJECT_URL: &str = "https://github.com/omni624562/ScreenRecord/";
const LATEST_URL: &str = "https://github.com/omni624562/ScreenRecord/releases/latest";
/// 只從本專案的 Release 下載新版 exe
const DOWNLOAD_PREFIX: &str = "https://github.com/omni624562/ScreenRecord/releases/download/";
const EXE_ASSET: &str = "ScreenRecorder.exe";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    /// 儲存庫不公開（查詢得到 404）：再查也一樣，本次執行不再自動檢查
    NotPublic,
    Other(String),
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateError::NotPublic => f.write_str("GitHub 儲存庫為私人（未登入無法查詢）"),
            UpdateError::Other(m) => f.write_str(m),
        }
    }
}

/// 比較 "1.2.0" 這類版本號：a > b 回傳正數
pub fn compare_versions(a: &str, b: &str) -> i64 {
    let parse = |v: &str| -> Vec<i64> {
        let v = v.strip_prefix(['v', 'V']).unwrap_or(v);
        v.split('-').next().unwrap_or("").split('.').map(|n| n.trim().parse::<i64>().unwrap_or(0)).collect()
    };
    let (x, y) = (parse(a), parse(b));
    for i in 0..x.len().max(y.len()) {
        let d = x.get(i).copied().unwrap_or(0) - y.get(i).copied().unwrap_or(0);
        if d != 0 {
            return d;
        }
    }
    0
}

#[derive(Deserialize, Default)]
struct Release {
    tag_name: Option<String>,
    html_url: Option<String>,
    published_at: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Deserialize, Default)]
struct Asset {
    #[serde(default)]
    name: String,
    #[serde(default)]
    browser_download_url: String,
    #[serde(default)]
    size: Option<u64>,
    /// GitHub 計算的 "sha256:<hex>"
    #[serde(default)]
    digest: Option<String>,
}

/// 可以在程式內更新的 exe：（網址, SHA-256, 大小）。網址必須是本專案的 Release，且一定要有 SHA-256
fn exe_asset(assets: &[Asset]) -> Option<(String, String, Option<u64>)> {
    let a = assets.iter().find(|a| a.name.eq_ignore_ascii_case(EXE_ASSET))?;
    if !a.browser_download_url.starts_with(DOWNLOAD_PREFIX) {
        return None;
    }
    let hex = a.digest.as_deref()?.strip_prefix("sha256:")?.to_ascii_lowercase();
    (hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit())).then(|| (a.browser_download_url.clone(), hex, a.size))
}

/// 由 HTTP 狀態碼與內容判斷（拆開方便測試）
pub fn parse_release(current: &str, status: u16, body: &str) -> Result<Option<UpdateInfo>, UpdateError> {
    // 儲存庫設為私人時，未登入的查詢一律得到 404
    if status == 404 {
        return Err(UpdateError::NotPublic);
    }
    if !(200..300).contains(&status) {
        return Err(UpdateError::Other(format!("HTTP {status}")));
    }
    let rel: Release = serde_json::from_str(body).map_err(|e| UpdateError::Other(e.to_string()))?;
    let Some(tag) = rel.tag_name.filter(|_| !rel.draft && !rel.prerelease) else { return Ok(None) };
    let version = tag.strip_prefix(['v', 'V']).unwrap_or(&tag).to_string();
    if compare_versions(&version, current) <= 0 {
        return Ok(None);
    }
    // 只接受指向本專案的網址（之後會交給 explorer 開啟）
    let url = rel.html_url.filter(|u| u.starts_with(PROJECT_URL)).unwrap_or_else(|| LATEST_URL.into());
    let exe = exe_asset(&rel.assets);
    Ok(Some(UpdateInfo { version, url, published_at: rel.published_at, download_url: exe.as_ref().map(|e| e.0.clone()), sha256: exe.as_ref().map(|e| e.1.clone()), size: exe.and_then(|e| e.2) }))
}

/// 有比 current 新的正式版就回傳它（同步，請在背景執行緒呼叫）
pub fn check_for_update(current: &str) -> Result<Option<UpdateInfo>, UpdateError> {
    let agent = crate::http::agent().timeout(Duration::from_secs(15)).build();
    let res = agent.get(RELEASES_API).set("Accept", "application/vnd.github+json").set("User-Agent", &format!("ScreenRecorder/{current}")).call();
    let (status, body) = match res {
        Ok(r) => (r.status(), r.into_string().unwrap_or_default()),
        Err(ureq::Error::Status(code, r)) => (code, r.into_string().unwrap_or_default()),
        Err(e) => return Err(UpdateError::Other(e.to_string())),
    };
    parse_release(current, status, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(compare_versions("1.2.0", "1.1.9") > 0);
        assert!(compare_versions("v1.10.0", "1.9.0") > 0);
        assert_eq!(compare_versions("1.2", "1.2.0"), 0);
        assert!(compare_versions("1.1.1", "1.2.0") < 0);
    }

    #[test]
    fn only_newer_stable_releases() {
        let url = "https://github.com/omni624562/ScreenRecord/releases/tag/v1.3.0";
        let body = |extra: &str| format!(r#"{{"tag_name":"v1.3.0","html_url":"{url}"{extra}}}"#);
        assert_eq!(parse_release("1.2.0", 200, &body("")), Ok(Some(UpdateInfo { version: "1.3.0".into(), url: url.into(), ..Default::default() })));
        assert_eq!(parse_release("1.3.0", 200, &body("")), Ok(None));
        assert_eq!(parse_release("1.2.0", 200, &body(r#","prerelease":true"#)), Ok(None));
        assert_eq!(parse_release("1.2.0", 200, &body(r#","draft":true"#)), Ok(None));
    }

    #[test]
    fn url_must_point_to_this_project() {
        let u = parse_release("1.0.0", 200, r#"{"tag_name":"v2.0.0","html_url":"https://evil.example/x"}"#).unwrap().unwrap();
        assert_eq!(u.url, LATEST_URL);
    }

    #[test]
    fn exe_asset_needs_project_url_and_sha256() {
        let hex = "ab".repeat(32);
        let body = |url: &str, digest: &str| {
            format!(
                r#"{{"tag_name":"v2.1.0","assets":[{{"name":"ScreenRecorder.zip","browser_download_url":"{DOWNLOAD_PREFIX}v2.1.0/ScreenRecorder.zip"}},{{"name":"ScreenRecorder.exe","browser_download_url":"{url}","size":9906688,"digest":"{digest}"}}]}}"#
            )
        };
        let ok_url = format!("{DOWNLOAD_PREFIX}v2.1.0/ScreenRecorder.exe");
        let u = parse_release("2.0.0", 200, &body(&ok_url, &format!("sha256:{}", hex.to_uppercase()))).unwrap().unwrap();
        assert_eq!(u.download_url.as_deref(), Some(ok_url.as_str()));
        assert_eq!(u.sha256.as_deref(), Some(hex.as_str()));
        assert_eq!(u.size, Some(9906688));
        // 其他網站的網址、沒有或格式不對的 SHA-256：只提示新版本，不提供程式內更新
        for (url, digest) in
            [("https://evil.example/ScreenRecorder.exe", format!("sha256:{hex}")), (ok_url.as_str(), String::new()), (ok_url.as_str(), "sha256:1234".into()), (ok_url.as_str(), format!("md5:{hex}"))]
        {
            let u = parse_release("2.0.0", 200, &body(url, &digest)).unwrap().unwrap();
            assert_eq!(u.download_url, None, "{url} {digest}");
            assert_eq!(u.sha256, None);
        }
    }

    #[test]
    fn errors() {
        assert_eq!(parse_release("1.0.0", 403, "{}"), Err(UpdateError::Other("HTTP 403".into())));
        assert_eq!(parse_release("1.0.0", 404, "{}"), Err(UpdateError::NotPublic));
        assert!(UpdateError::NotPublic.to_string().contains("私人"));
    }
}
