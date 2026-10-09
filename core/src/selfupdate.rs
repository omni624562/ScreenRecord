//! 程式內更新：下載新版 exe、核對大小與 SHA-256（由 GitHub Release 提供），再替換掉自己。
//!
//! Windows 不能覆寫執行中的 exe，但可以改名：目前的 exe 改名為 `.old`，新版放到原本的位置，
//! 接著啟動新版並結束自己；新版啟動時等舊的結束，再刪掉 `.old`。

use crate::types::UpdateInfo;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstallPhase {
    #[default]
    Idle,
    Downloading,
    Verifying,
    /// 已替換，正在啟動新版
    Restarting,
    Error,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallStatus {
    pub phase: InstallPhase,
    pub received: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// 取得下載內容：（大小, 內容）
pub type Fetch = Arc<dyn Fn(&str) -> Result<(Option<u64>, Box<dyn Read + Send>), String> + Send + Sync>;

fn http_fetch() -> Fetch {
    Arc::new(|url: &str| {
        let agent = crate::http::agent().timeout_connect(Duration::from_secs(15)).timeout_read(Duration::from_secs(60)).build();
        let res = agent.get(url).set("User-Agent", &format!("ScreenRecorder/{}", crate::version::APP_VERSION)).call().map_err(|e| e.to_string())?;
        let len = res.header("Content-Length").and_then(|v| v.parse().ok());
        Ok((len, Box::new(res.into_reader()) as Box<dyn Read + Send>))
    })
}

/// 同一資料夾內的暫存檔名（ScreenRecorder.exe → ScreenRecorder.exe.new / .old）
fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let mut s = exe.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// 新版啟動後刪掉舊版留下的 .old（還在執行時刪不掉，下次再刪）
pub fn cleanup_old(exe: &Path) {
    let _ = std::fs::remove_file(sibling(exe, ".old"));
    let _ = std::fs::remove_file(sibling(exe, ".new"));
}

/// 程式所在的資料夾能不能寫入（例如放在 Program Files 時不行，只能手動更新）
pub fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".screenrecorder-write-test-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"x").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

#[derive(Clone)]
pub struct Installer {
    st: Arc<Mutex<InstallStatus>>,
    fetch: Fetch,
}

impl Default for Installer {
    fn default() -> Self {
        Self::new(http_fetch())
    }
}

impl Installer {
    pub fn new(fetch: Fetch) -> Self {
        Installer { st: Arc::default(), fetch }
    }

    pub fn status(&self) -> InstallStatus {
        self.st.lock().unwrap().clone()
    }

    pub fn busy(&self) -> bool {
        matches!(self.status().phase, InstallPhase::Downloading | InstallPhase::Verifying | InstallPhase::Restarting)
    }

    fn set(&self, f: impl FnOnce(&mut InstallStatus)) {
        f(&mut self.st.lock().unwrap());
    }

    /// 開始下載並替換 exe；完成後呼叫 on_ready（啟動新版並結束自己）。回傳的執行緒用於測試等待
    pub fn start(&self, info: &UpdateInfo, exe: PathBuf, on_ready: impl FnOnce(PathBuf) + Send + 'static) -> Result<std::thread::JoinHandle<()>, String> {
        let (Some(url), Some(sha)) = (info.download_url.clone(), info.sha256.clone()) else {
            return Err("這個版本沒有提供可自動更新的檔案，請到下載頁面手動更新".into());
        };
        {
            let mut st = self.st.lock().unwrap();
            if matches!(st.phase, InstallPhase::Downloading | InstallPhase::Verifying | InstallPhase::Restarting) {
                return Err("正在更新中".into());
            }
            *st = InstallStatus { phase: InstallPhase::Downloading, total: info.size, version: Some(info.version.clone()), ..Default::default() };
        }
        let me = self.clone();
        let size = info.size;
        let version = info.version.clone();
        std::thread::Builder::new()
            .name("self-update".into())
            .spawn(move || match me.install(&url, &sha, size, &exe) {
                Ok(()) => {
                    crate::info!("[更新] 已替換為 v{version}，重新啟動");
                    me.set(|s| s.phase = InstallPhase::Restarting);
                    on_ready(exe);
                }
                Err(e) => {
                    crate::error!("[更新] 失敗：{e}");
                    me.set(|s| {
                        s.phase = InstallPhase::Error;
                        s.message = Some(e);
                    });
                }
            })
            .map_err(|e| e.to_string())
    }

    /// 啟動新版失敗時（例如被防毒擋下）：顯示錯誤
    pub fn fail(&self, message: String) {
        self.set(|s| {
            s.phase = InstallPhase::Error;
            s.message = Some(message);
        });
    }

    fn install(&self, url: &str, sha: &str, size: Option<u64>, exe: &Path) -> Result<(), String> {
        crate::info!("[更新] 下載 {url}");
        let new = sibling(exe, ".new");
        let result = self.download(url, sha, size, &new).and_then(|()| swap(exe, &new));
        if result.is_err() {
            let _ = std::fs::remove_file(&new);
        }
        result
    }

    fn download(&self, url: &str, sha: &str, size: Option<u64>, to: &Path) -> Result<(), String> {
        let (len, mut body) = (self.fetch)(url).map_err(|e| format!("無法下載新版：{e}"))?;
        if let Some(l) = len.or(size) {
            self.set(|s| s.total = Some(l));
        }
        let mut file = std::fs::File::create(to).map_err(|e| format!("無法寫入新版檔案：{e}"))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut received = 0u64;
        loop {
            let n = body.read(&mut buf).map_err(|e| format!("下載中斷：{e}"))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n]).map_err(|e| format!("無法寫入新版檔案：{e}"))?;
            received += n as u64;
            self.set(|s| s.received = received);
        }
        file.sync_all().map_err(|e| format!("無法寫入新版檔案：{e}"))?;
        drop(file);
        self.set(|s| s.phase = InstallPhase::Verifying);
        if size.is_some_and(|s| s != received) {
            return Err(format!("下載的檔案大小不符（{received} / {} bytes），已刪除", size.unwrap()));
        }
        let got: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if !got.eq_ignore_ascii_case(sha) {
            return Err("下載的檔案驗證失敗（SHA-256 不符），已刪除".into());
        }
        Ok(())
    }
}

/// 目前的 exe 改名為 .old、新版放到原位置；失敗時還原
fn swap(exe: &Path, new: &Path) -> Result<(), String> {
    let old = sibling(exe, ".old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old).map_err(|e| format!("無法替換程式檔案：{e}"))?;
    if let Err(e) = std::fs::rename(new, exe) {
        let _ = std::fs::rename(&old, exe);
        return Err(format!("無法替換程式檔案：{e}"));
    }
    Ok(())
}

/// 新版啟動時：等舊版（pid）結束，最多 timeout
pub fn wait_for_exit(pid: u32, timeout: Duration) {
    #[cfg(windows)]
    unsafe {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
        if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            WaitForSingleObject(h, timeout.as_millis().min(u32::MAX as u128) as u32);
            let _ = CloseHandle(h);
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (pid, timeout);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha_hex(b: &[u8]) -> String {
        Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
    }

    fn fetch_bytes(b: &'static [u8]) -> Fetch {
        Arc::new(move |_url: &str| Ok((Some(b.len() as u64), Box::new(std::io::Cursor::new(b)) as Box<dyn Read + Send>)))
    }

    fn info(sha: String, size: usize) -> UpdateInfo {
        UpdateInfo { version: "2.1.0".into(), download_url: Some("https://example/ScreenRecorder.exe".into()), sha256: Some(sha), size: Some(size as u64), ..Default::default() }
    }

    #[test]
    fn replaces_exe_and_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("ScreenRecorder.exe");
        std::fs::write(&exe, b"old version").unwrap();
        let new: &'static [u8] = b"new version bytes";
        let inst = Installer::new(fetch_bytes(new));
        let (tx, rx) = std::sync::mpsc::channel();
        inst.start(&info(sha_hex(new), new.len()), exe.clone(), move |p| tx.send(p).unwrap()).unwrap().join().unwrap();
        assert_eq!(rx.try_recv().unwrap(), exe);
        assert_eq!(std::fs::read(&exe).unwrap(), new);
        assert_eq!(std::fs::read(sibling(&exe, ".old")).unwrap(), b"old version");
        assert!(!sibling(&exe, ".new").exists());
        let st = inst.status();
        assert_eq!(st.phase, InstallPhase::Restarting);
        assert_eq!(st.received, new.len() as u64);
        assert_eq!(st.version.as_deref(), Some("2.1.0"));
        // 新版啟動後清掉 .old
        cleanup_old(&exe);
        assert!(!sibling(&exe, ".old").exists());
    }

    #[test]
    fn rejects_tampered_or_truncated_download() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("ScreenRecorder.exe");
        std::fs::write(&exe, b"old version").unwrap();
        for (expected, size, msg) in [(sha_hex(b"something else"), 8, "SHA-256"), (sha_hex(b"tampered"), 999, "大小不符")] {
            let inst = Installer::new(fetch_bytes(b"tampered"));
            inst.start(&info(expected, size), exe.clone(), |_| panic!("不應該重新啟動")).unwrap().join().unwrap();
            let st = inst.status();
            assert_eq!(st.phase, InstallPhase::Error);
            assert!(st.message.unwrap().contains(msg));
            assert_eq!(std::fs::read(&exe).unwrap(), b"old version"); // 原本的程式不動
            assert!(!sibling(&exe, ".new").exists());
            assert!(!sibling(&exe, ".old").exists());
        }
    }

    #[test]
    fn needs_download_info_and_one_at_a_time() {
        let inst = Installer::new(fetch_bytes(b""));
        let no_file = UpdateInfo { version: "2.1.0".into(), ..Default::default() };
        assert!(inst.start(&no_file, PathBuf::from("x.exe"), |_| {}).unwrap_err().contains("手動更新"));
        inst.set(|s| s.phase = InstallPhase::Downloading);
        assert!(inst.busy());
        assert!(inst.start(&info(sha_hex(b""), 0), PathBuf::from("x.exe"), |_| {}).unwrap_err().contains("正在更新"));
    }

    #[test]
    fn writable_check() {
        let dir = tempfile::tempdir().unwrap();
        assert!(dir_writable(dir.path()));
        assert!(!dir_writable(&dir.path().join("missing")));
    }
}
