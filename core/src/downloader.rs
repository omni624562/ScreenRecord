//! 自動下載 FFmpeg（使用者按下按鈕才執行）。
//!
//! - 鎖定版本：固定下載 FFmpeg 9.0.2 essentials（gyan.dev），本程式已用這個版本測試過
//! - SHA-256 內建在程式裡：即使下載來源被入侵或檔案被調包也會被擋下
//! - 主來源失敗時改用 GitHub 上的同一個檔案（同樣比對內建的 SHA-256）
//! - 用 Windows 內建的 tar（System32，不受 PATH 上 Git / MSYS2 的 GNU tar 影響）解出 bin\ffmpeg.exe，
//!   確認能執行後才放到定位：exe 所在資料夾可寫入就放那裡，否則放 %LOCALAPPDATA%\ScreenRecorder

use crate::paths::{app_dir, data_dir};
use crate::process::RunResult;
use crate::types::{DownloadPhase, DownloadStatus};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const FFMPEG_VERSION: &str = "9.0.2";
pub const FFMPEG_SHA256: &str = "60f467265b1e312373dbcd92200c2618a74850f98d3d078e94296bb3fa2047ba";

pub fn ffmpeg_urls() -> Vec<String> {
    vec![
        format!("https://www.gyan.dev/ffmpeg/builds/packages/ffmpeg-{FFMPEG_VERSION}-essentials_build.zip"),
        format!("https://github.com/GyanD/codexffmpeg/releases/download/{FFMPEG_VERSION}/ffmpeg-{FFMPEG_VERSION}-essentials_build.zip"),
    ]
}

/// Windows 內建 tar（bsdtar，支援 zip）
pub fn system_tar() -> PathBuf {
    let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\Windows"));
    root.join("System32").join("tar.exe")
}

/// 可以寫入的放置位置：優先 exe 旁邊
pub fn download_target() -> Result<PathBuf, String> {
    for dir in [app_dir(), data_dir()] {
        let probe = dir.join(format!(".write-test-{}", std::process::id()));
        if std::fs::create_dir_all(&dir).is_ok() && std::fs::write(&probe, "").is_ok() {
            let _ = std::fs::remove_file(&probe);
            return Ok(dir.join(if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" }));
        }
    }
    Err("找不到可寫入的資料夾".into())
}

/// 網路回應：內容長度（可能未知）與內容
pub type Body = (Option<u64>, Box<dyn Read + Send>);

pub type FetchFn = Box<dyn Fn(&str) -> Result<Body, String> + Send + Sync>;
pub type RunFn = Box<dyn Fn(&Path, &[String], Duration) -> RunResult + Send + Sync>;

/// 可替換的外部相依（測試時不真的連網、不真的執行 tar）
pub struct Deps {
    pub fetch: FetchFn,
    pub run: RunFn,
    pub target: Box<dyn Fn() -> Result<PathBuf, String> + Send + Sync>,
    pub urls: Vec<String>,
    pub sha256: String,
    pub tar: PathBuf,
}

impl Default for Deps {
    fn default() -> Self {
        Self {
            fetch: Box::new(|url| {
                let agent = crate::http::agent().timeout_connect(Duration::from_secs(20)).timeout_read(Duration::from_secs(60)).build();
                match agent.get(url).call() {
                    Ok(r) => {
                        let len = r.header("content-length").and_then(|v| v.parse().ok());
                        Ok((len, Box::new(r.into_reader())))
                    }
                    Err(ureq::Error::Status(code, _)) => Err(format!("下載失敗（HTTP {code}）")),
                    Err(e) => Err(format!("下載失敗：{e}")),
                }
            }),
            run: Box::new(run_blocking),
            target: Box::new(download_target),
            urls: ffmpeg_urls(),
            sha256: FFMPEG_SHA256.into(),
            tar: system_tar(),
        }
    }
}

/// 同步執行指令（下載在背景執行緒裡進行）
pub fn run_blocking(program: &Path, args: &[String], timeout: Duration) -> RunResult {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return RunResult { code: -1, stderr: e.to_string(), ..Default::default() },
    };
    let (mut out, mut err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let t_out = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = out.read_to_end(&mut s);
        s
    });
    let t_err = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = err.read_to_end(&mut s);
        s
    });
    let start = Instant::now();
    let (code, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(st)) => break (st.code().unwrap_or(-1), false),
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                break (-1, true);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => break (-1, false),
        }
    };
    RunResult {
        code,
        stdout: String::from_utf8_lossy(&t_out.join().unwrap_or_default()).into_owned(),
        stderr: String::from_utf8_lossy(&t_err.join().unwrap_or_default()).into_owned(),
        timed_out,
    }
}

struct Inner {
    st: DownloadStatus,
    cancel: Option<Arc<AtomicBool>>,
    job: Option<std::thread::JoinHandle<()>>,
}

#[derive(Clone)]
pub struct Downloader {
    inner: Arc<Mutex<Inner>>,
    deps: Arc<Deps>,
}

enum Fail {
    Canceled,
    Error(String),
}

impl Downloader {
    pub fn new(deps: Deps) -> Self {
        Self { inner: Arc::new(Mutex::new(Inner { st: DownloadStatus::default(), cancel: None, job: None })), deps: Arc::new(deps) }
    }

    pub fn status(&self) -> DownloadStatus {
        self.inner.lock().unwrap().st.clone()
    }

    pub fn busy(&self) -> bool {
        matches!(self.status().phase, DownloadPhase::Downloading | DownloadPhase::Verifying | DownloadPhase::Extracting)
    }

    /// 開始下載；on_done 在安裝完成後呼叫（例如重新偵測 FFmpeg），失敗不影響「已安裝」的結果
    pub fn start(&self, on_done: impl FnOnce() -> Result<(), String> + Send + 'static) {
        let mut g = self.inner.lock().unwrap();
        if matches!(g.st.phase, DownloadPhase::Downloading | DownloadPhase::Verifying | DownloadPhase::Extracting) {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        g.cancel = Some(cancel.clone());
        g.st = DownloadStatus { phase: DownloadPhase::Downloading, ..Default::default() };
        let me = self.clone();
        g.job = Some(std::thread::spawn(move || match me.install(&cancel) {
            Ok(()) => {
                if let Err(e) = on_done() {
                    crate::error!("[下載 FFmpeg] 安裝完成，但重新偵測失敗：{e}");
                }
            }
            Err(Fail::Canceled) => {}
            Err(Fail::Error(msg)) => {
                let mut g = me.inner.lock().unwrap();
                if g.st.phase != DownloadPhase::Canceled {
                    g.st.phase = DownloadPhase::Error;
                    g.st.message = Some(msg.clone());
                    crate::error!("[下載 FFmpeg] 失敗：{msg}");
                }
            }
        }));
    }

    pub fn cancel(&self) {
        let mut g = self.inner.lock().unwrap();
        if !matches!(g.st.phase, DownloadPhase::Downloading | DownloadPhase::Verifying | DownloadPhase::Extracting) {
            return;
        }
        g.st.phase = DownloadPhase::Canceled;
        g.st.message = Some("已取消下載".into());
        if let Some(c) = &g.cancel {
            c.store(true, Ordering::SeqCst);
        }
    }

    /// 等目前的工作結束（測試用）
    pub fn join(&self) {
        let job = self.inner.lock().unwrap().job.take();
        if let Some(j) = job {
            let _ = j.join();
        }
    }

    fn set(&self, f: impl FnOnce(&mut DownloadStatus)) {
        f(&mut self.inner.lock().unwrap().st);
    }

    fn install(&self, cancel: &AtomicBool) -> Result<(), Fail> {
        let target = (self.deps.target)().map_err(Fail::Error)?;
        let work = std::env::temp_dir().join(format!("ScreenRecorder-ffmpeg-{}-{}", crate::paths::now_ms(), std::process::id()));
        std::fs::create_dir_all(&work).map_err(|e| Fail::Error(e.to_string()))?;
        self.set(|s| s.target = Some(target.display().to_string()));
        let r = self.install_in(&work, &target, cancel);
        let _ = std::fs::remove_dir_all(&work);
        r
    }

    fn install_in(&self, work: &Path, target: &Path, cancel: &AtomicBool) -> Result<(), Fail> {
        let zip = work.join("ffmpeg.zip");
        // 1. 下載並比對內建的 SHA-256；主來源失敗就換下一個
        let mut last_error = None;
        let mut ok = false;
        for url in &self.deps.urls {
            crate::info!("[下載 FFmpeg] {url} → {}", target.display());
            match self.download_to(url, &zip, cancel) {
                Ok(()) => {
                    ok = true;
                    break;
                }
                Err(Fail::Canceled) => return Err(Fail::Canceled),
                Err(Fail::Error(e)) => {
                    if cancel.load(Ordering::SeqCst) {
                        return Err(Fail::Canceled);
                    }
                    crate::error!("[下載 FFmpeg] {url} 失敗：{e}");
                    last_error = Some(e);
                }
            }
        }
        if !ok {
            return Err(Fail::Error(last_error.unwrap_or_else(|| "下載失敗".into())));
        }

        // 2. 解壓縮：只取 bin\ffmpeg.exe
        self.set(|s| s.phase = DownloadPhase::Extracting);
        let tar = &self.deps.tar;
        let zip_s = zip.display().to_string();
        let list = (self.deps.run)(tar, &["-tf".into(), zip_s.clone()], Duration::from_secs(60));
        if list.code != 0 {
            return Err(Fail::Error(format!("無法讀取壓縮檔：{}", list.stderr.trim())));
        }
        let entry = list
            .stdout
            .lines()
            .map(str::trim)
            .find(|l| {
                let lower = l.to_lowercase();
                lower == "bin/ffmpeg.exe" || lower.ends_with("/bin/ffmpeg.exe")
            })
            .ok_or_else(|| Fail::Error("壓縮檔裡找不到 bin/ffmpeg.exe".into()))?
            .to_string();
        let ex = (self.deps.run)(tar, &["-xf".into(), zip_s, "-C".into(), work.display().to_string(), entry.clone()], Duration::from_secs(120));
        let extracted = entry.split('/').fold(work.to_path_buf(), |p, part| p.join(part));
        if ex.code != 0 || !extracted.exists() {
            return Err(Fail::Error(format!("解壓縮失敗：{}", ex.stderr.trim())));
        }
        if cancel.load(Ordering::SeqCst) {
            return Err(Fail::Canceled);
        }

        // 3. 確認能執行再放到定位（先放暫存名稱再改名，避免留下半個檔案）
        let ver = (self.deps.run)(&extracted, &["-hide_banner".into(), "-version".into()], Duration::from_secs(15));
        if ver.code != 0 {
            return Err(Fail::Error("下載的 ffmpeg.exe 無法執行".into()));
        }
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Fail::Error(e.to_string()))?;
        }
        let staging = PathBuf::from(format!("{}.download", target.display()));
        std::fs::copy(&extracted, &staging).map_err(|e| Fail::Error(e.to_string()))?;
        let _ = std::fs::remove_file(target);
        std::fs::rename(&staging, target).map_err(|e| Fail::Error(e.to_string()))?;
        let version = crate::ffmpeg::parse_version(&ver.stdout);
        let msg = format!("已安裝 FFmpeg {}：{}", version.clone().unwrap_or_default(), target.display());
        self.set(|s| {
            s.version = version;
            s.phase = DownloadPhase::Done;
            s.message = Some(msg.clone());
        });
        crate::info!("[下載 FFmpeg] {msg}");
        Ok(())
    }

    /// 下載單一來源到 zip（邊下載邊計算雜湊與進度），並比對內建的 SHA-256
    fn download_to(&self, url: &str, zip: &Path, cancel: &AtomicBool) -> Result<(), Fail> {
        let started = Instant::now();
        self.set(|s| {
            s.phase = DownloadPhase::Downloading;
            s.received = 0;
            s.total = None;
            s.speed = None;
        });
        let (len, mut body) = (self.deps.fetch)(url).map_err(Fail::Error)?;
        self.set(|s| s.total = len.filter(|&n| n > 0));
        let mut file = std::fs::File::create(zip).map_err(|e| Fail::Error(e.to_string()))?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err(Fail::Canceled);
            }
            let n = body.read(&mut buf).map_err(|e| if cancel.load(Ordering::SeqCst) { Fail::Canceled } else { Fail::Error(format!("下載中斷：{e}")) })?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n]).map_err(|e| Fail::Error(e.to_string()))?;
            let sec = started.elapsed().as_secs_f64();
            self.set(|s| {
                s.received += n as u64;
                s.speed = (sec > 0.5).then(|| s.received as f64 / sec);
            });
        }
        drop(file);
        if cancel.load(Ordering::SeqCst) {
            return Err(Fail::Canceled);
        }
        self.set(|s| s.phase = DownloadPhase::Verifying);
        let digest: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if digest != self.deps.sha256.to_lowercase() {
            return Err(Fail::Error("檔案的 SHA-256 與預期不符（下載不完整或檔案遭替換），已刪除".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    const ENTRY: &str = "ffmpeg-9.0.2-essentials_build/bin/ffmpeg.exe";

    fn zip_bytes() -> Vec<u8> {
        "fake zip content ".repeat(1000).into_bytes()
    }
    fn sha(b: &[u8]) -> String {
        Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
    }

    /// 模擬 tar（列出 / 解壓）與執行 ffmpeg -version
    fn fake_run(program: &Path, args: &[String], _t: Duration) -> RunResult {
        let ok = |stdout: &str| RunResult { code: 0, stdout: stdout.into(), ..Default::default() };
        let _ = program;
        match args.first().map(String::as_str) {
            Some("-tf") => ok(&format!("ffmpeg-9.0.2-essentials_build/\n{ENTRY}\n")),
            Some("-xf") => {
                let out = ENTRY.split('/').fold(PathBuf::from(&args[3]), |p, s| p.join(s));
                std::fs::create_dir_all(out.parent().unwrap()).unwrap();
                std::fs::write(out, "FAKE-FFMPEG-EXE").unwrap();
                ok("")
            }
            _ => ok("ffmpeg version 9.0.2-test Copyright"),
        }
    }

    struct Setup {
        dl: Downloader,
        target: PathBuf,
        fetched: Arc<Mutex<Vec<String>>>,
        _dir: tempfile::TempDir,
    }

    fn setup(fetch: Option<FetchFn>, run: Option<RunFn>) -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out").join("ffmpeg.exe");
        let fetched: Arc<Mutex<Vec<String>>> = Arc::default();
        let f2 = fetched.clone();
        let t2 = target.clone();
        let fetch = fetch.unwrap_or_else(|| {
            Box::new(move |url: &str| {
                f2.lock().unwrap().push(url.into());
                let z = zip_bytes();
                Ok((Some(z.len() as u64), Box::new(std::io::Cursor::new(z)) as Box<dyn Read + Send>))
            })
        });
        let dl = Downloader::new(Deps {
            fetch,
            run: run.unwrap_or_else(|| Box::new(fake_run)),
            target: Box::new(move || Ok(t2.clone())),
            urls: vec!["https://primary/ffmpeg.zip".into(), "https://mirror/ffmpeg.zip".into()],
            sha256: sha(&zip_bytes()),
            tar: PathBuf::from("tar.exe"),
        });
        Setup { dl, target, fetched, _dir: dir }
    }

    #[test]
    fn success_installs_and_notifies() {
        let s = setup(None, None);
        let done = Arc::new(AtomicUsize::new(0));
        let d2 = done.clone();
        s.dl.start(move || {
            d2.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        s.dl.join();
        let st = s.dl.status();
        assert_eq!(st.phase, DownloadPhase::Done, "{:?}", st.message);
        assert_eq!(st.version.as_deref(), Some("9.0.2-test"));
        assert_eq!(std::fs::read_to_string(&s.target).unwrap(), "FAKE-FFMPEG-EXE");
        assert!(!PathBuf::from(format!("{}.download", s.target.display())).exists());
        assert_eq!(*s.fetched.lock().unwrap(), vec!["https://primary/ffmpeg.zip".to_string()]);
        assert_eq!(done.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn falls_back_to_mirror_when_primary_is_tampered() {
        let fetched: Arc<Mutex<Vec<String>>> = Arc::default();
        let f2 = fetched.clone();
        let s = setup(
            Some(Box::new(move |url: &str| {
                f2.lock().unwrap().push(url.into());
                let body = if url.contains("primary") { b"tampered".to_vec() } else { zip_bytes() };
                Ok((None, Box::new(std::io::Cursor::new(body)) as Box<dyn Read + Send>))
            })),
            None,
        );
        s.dl.start(|| Ok(()));
        s.dl.join();
        assert_eq!(s.dl.status().phase, DownloadPhase::Done);
        assert!(s.target.exists());
        assert_eq!(*fetched.lock().unwrap(), vec!["https://primary/ffmpeg.zip".to_string(), "https://mirror/ffmpeg.zip".to_string()]);
    }

    #[test]
    fn all_sources_tampered_fails_without_files() {
        let s = setup(Some(Box::new(|_: &str| Ok((None, Box::new(std::io::Cursor::new(b"tampered".to_vec())) as Box<dyn Read + Send>)))), None);
        s.dl.start(|| Ok(()));
        s.dl.join();
        let st = s.dl.status();
        assert_eq!(st.phase, DownloadPhase::Error);
        assert!(st.message.unwrap().contains("SHA-256"));
        assert!(!s.target.exists());
    }

    /// 每次讀取都等一下的內容（模擬慢速下載）
    struct Slow;
    impl Read for Slow {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            std::thread::sleep(Duration::from_millis(20));
            let n = buf.len().min(1024);
            buf[..n].fill(0);
            Ok(n)
        }
    }

    #[test]
    fn cancel_while_downloading() {
        let s = setup(Some(Box::new(|_: &str| Ok((None, Box::new(Slow) as Box<dyn Read + Send>)))), None);
        s.dl.start(|| Ok(()));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(s.dl.status().phase, DownloadPhase::Downloading);
        assert!(s.dl.status().received > 0);
        s.dl.cancel();
        s.dl.join();
        assert_eq!(s.dl.status().phase, DownloadPhase::Canceled);
        assert!(!s.target.exists());
    }

    #[test]
    fn refresh_failure_still_counts_as_installed() {
        let s = setup(None, None);
        s.dl.start(|| Err("refresh boom".into()));
        s.dl.join();
        assert_eq!(s.dl.status().phase, DownloadPhase::Done);
        assert!(s.target.exists());
    }

    #[test]
    fn archive_without_ffmpeg() {
        let s = setup(None, Some(Box::new(|_: &Path, _: &[String], _| RunResult { code: 0, stdout: "readme.txt\n".into(), ..Default::default() })));
        s.dl.start(|| Ok(()));
        s.dl.join();
        let st = s.dl.status();
        assert_eq!(st.phase, DownloadPhase::Error);
        assert!(st.message.unwrap().contains("bin/ffmpeg.exe"));
        assert!(!s.target.exists());
    }
}
