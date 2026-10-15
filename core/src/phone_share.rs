//! 傳到手機：在這台電腦開一個暫時的下載網頁（只在區域網路），手機掃 QR 碼就能看、下載檔案。
//! 網址裡有一段隨機的代碼，猜不到；一段時間後或關掉視窗就停止。不經過網路上的任何伺服器。

use crate::{tr, trf};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// 分享多久後自動停止
pub const LIFETIME: Duration = Duration::from_secs(30 * 60);

/// 正在分享的檔案；丟掉（drop）就停止
pub struct Share {
    pub url: String,
    downloads: Arc<AtomicUsize>,
    until: Instant,
    task: tokio::task::JoinHandle<()>,
}

impl Share {
    /// 手機下載了幾次
    pub fn downloads(&self) -> usize {
        self.downloads.load(Ordering::Relaxed)
    }

    /// 還剩多久自動停止（已經停止時是 0）
    pub fn remaining(&self) -> Duration {
        if self.task.is_finished() {
            return Duration::ZERO;
        }
        self.until.saturating_duration_since(Instant::now())
    }

    pub fn is_running(&self) -> bool {
        !self.task.is_finished()
    }
}

impl Drop for Share {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// 開始分享（要在 tokio 執行環境裡呼叫）
pub async fn start(path: &Path) -> crate::Result<Share> {
    let meta = tokio::fs::metadata(path).await.map_err(|_| crate::Error::config(tr!("找不到這個檔案", "Couldn't find this file")))?;
    if !meta.is_file() {
        return Err(crate::Error::config(tr!("找不到這個檔案", "Couldn't find this file")));
    }
    let ip = lan_ip()
        .ok_or_else(|| crate::Error::config(tr!("這台電腦沒有連上區域網路（Wi-Fi 或網路線），手機連不到", "This computer isn't on a local network (Wi-Fi or cable), so your phone can't reach it")))?;
    let listener = TcpListener::bind(("0.0.0.0", 0)).await.map_err(|e| crate::Error::config(trf!("無法開始分享：{e}", "Couldn't start sharing: {e}")))?;
    let port = listener.local_addr().map_err(|e| crate::Error::config(e.to_string()))?.port();
    let token = token();
    let page = page(path, meta.len(), &token);
    let downloads = Arc::new(AtomicUsize::new(0));
    let until = Instant::now() + LIFETIME;
    let ctx = Arc::new(Ctx { path: path.to_path_buf(), size: meta.len(), token: token.clone(), page, downloads: downloads.clone() });
    let deadline = tokio::time::Instant::from_std(until);
    let task = tokio::spawn(async move {
        let mut conns = tokio::task::JoinSet::new();
        loop {
            let accepted = tokio::select! {
                r = listener.accept() => r,
                _ = tokio::time::sleep_until(deadline) => break,
            };
            let Ok((sock, _)) = accepted else { continue };
            let ctx = ctx.clone();
            conns.spawn(async move {
                let _ = tokio::time::timeout(Duration::from_secs(60 * 60), serve(sock, &ctx)).await;
            });
            // 清掉已經結束的連線
            while conns.try_join_next().is_some() {}
        }
        // 時間到：還在下載的也一起停止（JoinSet 丟掉時會中止）
    });
    crate::log::write("info", "開始傳到手機（區域網路分享）");
    Ok(Share { url: format!("http://{ip}:{port}/{token}"), downloads, until, task })
}

struct Ctx {
    path: PathBuf,
    size: u64,
    token: String,
    page: String,
    downloads: Arc<AtomicUsize>,
}

/// 這台電腦在區域網路上的位址：問系統要連到外面時會從哪個位址出去（UDP connect 不會真的送出封包）
fn lan_ip() -> Option<std::net::Ipv4Addr> {
    for probe in ["8.8.8.8:80", "192.168.0.1:80", "10.0.0.1:80"] {
        let Ok(s) = std::net::UdpSocket::bind("0.0.0.0:0") else { continue };
        if s.connect(probe).is_err() {
            continue;
        }
        if let Ok(std::net::SocketAddr::V4(a)) = s.local_addr() {
            let ip = *a.ip();
            if !ip.is_loopback() && !ip.is_unspecified() && !ip.is_link_local() {
                return Some(ip);
            }
        }
    }
    None
}

/// 網址裡的隨機代碼（128 位元）
fn token() -> String {
    use sha2::{Digest, Sha256};
    use std::hash::{BuildHasher, Hasher};
    let mut h = Sha256::new();
    for _ in 0..4 {
        let mut x = std::collections::hash_map::RandomState::new().build_hasher();
        x.write_u64(0);
        h.update(x.finish().to_le_bytes());
    }
    h.update(format!("{:?}{}", std::time::SystemTime::now(), std::process::id()).as_bytes());
    h.update((&h as *const _ as usize).to_le_bytes());
    h.finalize()[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// 依副檔名的類型
fn mime(path: &Path) -> &'static str {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "bmp" => "image/bmp",
        "m4a" => "audio/mp4",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "pdf" => "application/pdf",
        "html" | "htm" => "text/html; charset=utf-8",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "srt" | "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// 網址用的百分比編碼（檔名）
fn pct(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

/// 手機上看到的下載頁
fn page(path: &Path, size: u64, token: &str) -> String {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let m = mime(path);
    let preview = if m.starts_with("image/") {
        format!(r#"<img src="/{token}/f" alt="">"#)
    } else if m.starts_with("video/") {
        format!(r#"<video src="/{token}/f" controls playsinline preload="metadata"></video>"#)
    } else if m.starts_with("audio/") {
        format!(r#"<audio src="/{token}/f" controls preload="metadata"></audio>"#)
    } else {
        String::new()
    };
    let (lang, download, hint) = if crate::i18n::is_en() {
        ("en", "Download", "Sent from ScreenRecorder on your computer. This page stops working when sharing ends.")
    } else {
        ("zh-Hant-TW", "下載", "從電腦上的螢幕錄影程式傳來。電腦停止分享後，這個網頁就打不開了。")
    };
    format!(
        r#"<!doctype html><html lang="{lang}"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{title}</title><style>
:root{{color-scheme:light dark;--bg:#f6f7f9;--card:#fff;--text:#1d1f23;--muted:#6b7280;--accent:#2563eb}}
@media (prefers-color-scheme:dark){{:root{{--bg:#16181c;--card:#202329;--text:#eceef2;--muted:#9aa1ad;--accent:#4f8cff}}}}
body{{margin:0;background:var(--bg);color:var(--text);font:16px/1.5 system-ui,-apple-system,"Segoe UI","Microsoft JhengHei",sans-serif}}
main{{max-width:720px;margin:0 auto;padding:20px 16px 40px}}
.card{{background:var(--card);border-radius:14px;padding:16px;box-shadow:0 1px 3px rgba(0,0,0,.08)}}
h1{{font-size:18px;margin:0 0 4px;word-break:break-all}}
.size{{color:var(--muted);font-size:14px;margin-bottom:12px}}
img,video{{display:block;width:100%;max-height:70vh;object-fit:contain;border-radius:8px;background:#000;margin-bottom:14px}}
audio{{width:100%;margin-bottom:14px}}
a.btn{{display:block;text-align:center;background:var(--accent);color:#fff;text-decoration:none;font-weight:600;padding:14px;border-radius:10px;font-size:17px}}
p{{color:var(--muted);font-size:13px;margin:14px 2px 0}}
</style></head><body><main><div class="card"><h1>{title}</h1><div class="size">{size}</div>{preview}<a class="btn" href="/{token}/d">{download}</a></div><p>{hint}</p></main></body></html>"#,
        title = esc(name),
        size = crate::format::format_bytes(size),
    )
}

/// 處理一個連線（一個請求，回完就關）
async fn serve(mut sock: TcpStream, ctx: &Ctx) -> std::io::Result<()> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let head_end = loop {
        let n = sock.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > 16 * 1024 {
            return respond(&mut sock, "431 Request Header Fields Too Large", "text/plain", b"").await;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let (method, target) = (first.next().unwrap_or(""), first.next().unwrap_or(""));
    let range = lines.filter_map(|l| l.split_once(':')).find(|(k, _)| k.trim().eq_ignore_ascii_case("range")).map(|(_, v)| v.trim().to_string());
    let head_only = method == "HEAD";
    if method != "GET" && !head_only {
        return respond(&mut sock, "405 Method Not Allowed", "text/plain", b"").await;
    }
    let path = target.split('?').next().unwrap_or("");
    let base = format!("/{}", ctx.token);
    if path == base || path == format!("{base}/") {
        let body = if head_only { &b""[..] } else { ctx.page.as_bytes() };
        return respond(&mut sock, "200 OK", "text/html; charset=utf-8", body).await;
    }
    let attach = if path == format!("{base}/f") {
        false
    } else if path == format!("{base}/d") {
        true
    } else {
        return respond(&mut sock, "404 Not Found", "text/plain", b"Not found").await;
    };
    send_file(&mut sock, ctx, attach, range.as_deref(), head_only).await
}

async fn respond(sock: &mut TcpStream, status: &str, ctype: &str, body: &[u8]) -> std::io::Result<()> {
    let head = format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", body.len());
    sock.write_all(head.as_bytes()).await?;
    sock.write_all(body).await?;
    sock.shutdown().await
}

/// Range: bytes=a-b → 要送的範圍（含頭含尾）；格式不對或超出就送整個檔案
fn parse_range(v: &str, size: u64) -> Option<(u64, u64)> {
    let spec = v.strip_prefix("bytes=")?;
    if spec.contains(',') || size == 0 {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (start, end) = if a.is_empty() {
        let n: u64 = b.parse().ok()?;
        (size.saturating_sub(n), size - 1)
    } else {
        let s: u64 = a.parse().ok()?;
        let e = if b.is_empty() { size - 1 } else { b.parse::<u64>().ok()?.min(size - 1) };
        (s, e)
    };
    (start <= end && start < size).then_some((start, end))
}

async fn send_file(sock: &mut TcpStream, ctx: &Ctx, attach: bool, range: Option<&str>, head_only: bool) -> std::io::Result<()> {
    let mut file = match tokio::fs::File::open(&ctx.path).await {
        Ok(f) => f,
        Err(_) => return respond(sock, "404 Not Found", "text/plain", b"Not found").await,
    };
    let size = ctx.size;
    let part = range.and_then(|r| parse_range(r, size));
    let (start, end) = part.unwrap_or((0, size.saturating_sub(1)));
    let len = if size == 0 { 0 } else { end - start + 1 };
    let name = ctx.path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let ascii: String = name.chars().map(|c| if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' { c } else { '_' }).collect();
    let disp = format!("{}; filename=\"{ascii}\"; filename*=UTF-8''{}", if attach { "attachment" } else { "inline" }, pct(name));
    let mut head = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {len}\r\nAccept-Ranges: bytes\r\nContent-Disposition: {disp}\r\nCache-Control: no-store\r\nConnection: close\r\n",
        if part.is_some() { "206 Partial Content" } else { "200 OK" },
        mime(&ctx.path),
    );
    if part.is_some() {
        head.push_str(&format!("Content-Range: bytes {start}-{end}/{size}\r\n"));
    }
    head.push_str("\r\n");
    sock.write_all(head.as_bytes()).await?;
    if head_only || len == 0 {
        return sock.shutdown().await;
    }
    // 從頭開始的下載才算一次（影片預覽會分好幾段讀）
    if attach && start == 0 {
        ctx.downloads.fetch_add(1, Ordering::Relaxed);
    }
    file.seek(std::io::SeekFrom::Start(start)).await?;
    let mut left = len;
    let mut buf = vec![0u8; 256 * 1024];
    while left > 0 {
        let want = (buf.len() as u64).min(left) as usize;
        let n = file.read(&mut buf[..want]).await?;
        if n == 0 {
            break;
        }
        sock.write_all(&buf[..n]).await?;
        left -= n as u64;
    }
    sock.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_range("bytes=500-", 1000), Some((500, 999)));
        assert_eq!(parse_range("bytes=-100", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=900-5000", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=1000-", 1000), None);
        assert_eq!(parse_range("bytes=0-1,5-9", 1000), None);
        assert_eq!(parse_range("items=0-1", 1000), None);
    }

    #[test]
    fn tokens_differ() {
        let (a, b) = (token(), token());
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }

    #[test]
    fn names_are_encoded() {
        assert_eq!(pct("錄影 1.mp4"), "%E9%8C%84%E5%BD%B1%201.mp4");
        assert!(page(Path::new("a<b>.png"), 10, "t").contains("a&lt;b&gt;.png"));
    }

    async fn get(addr: &str, req: &str) -> Vec<u8> {
        let mut s = TcpStream::connect(addr).await.unwrap();
        s.write_all(req.as_bytes()).await.unwrap();
        let mut out = vec![];
        s.read_to_end(&mut out).await.unwrap();
        out
    }

    #[tokio::test]
    async fn serves_page_file_and_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("影片.mp4");
        let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&file, &data).unwrap();
        let Ok(share) = start(&file).await else {
            // 測試環境沒有網路介面：略過
            return;
        };
        let url = share.url.strip_prefix("http://").unwrap();
        let (host, path) = url.split_once('/').unwrap();
        let port = host.split(':').nth(1).unwrap();
        let addr = format!("127.0.0.1:{port}");

        let page = String::from_utf8(get(&addr, &format!("GET /{path} HTTP/1.1\r\nHost: x\r\n\r\n")).await).unwrap();
        assert!(page.starts_with("HTTP/1.1 200"), "{page}");
        assert!(page.contains("影片.mp4") && page.contains("<video"));
        // 看手機上的樣子：PHONE_PAGE_OUT=資料夾（網頁與影片一起存下來）
        if let Ok(out) = std::env::var("PHONE_PAGE_OUT") {
            let body = &page[page.find("<!doctype").unwrap()..];
            std::fs::write(std::path::Path::new(&out).join("page.html"), body.replace(&format!("/{path}/f"), "preview.png")).unwrap();
        }

        let bad = String::from_utf8_lossy(&get(&addr, "GET /nope HTTP/1.1\r\n\r\n").await).to_string();
        assert!(bad.starts_with("HTTP/1.1 404"));

        let full = get(&addr, &format!("GET /{path}/d HTTP/1.1\r\n\r\n")).await;
        let i = full.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8_lossy(&full[..i]);
        assert!(head.contains("attachment") && head.contains("filename*=UTF-8''%E5%BD%B1%E7%89%87.mp4"), "{head}");
        assert_eq!(&full[i + 4..], &data[..]);
        assert_eq!(share.downloads(), 1);

        let part = get(&addr, &format!("GET /{path}/f HTTP/1.1\r\nRange: bytes=100-199\r\n\r\n")).await;
        let i = part.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8_lossy(&part[..i]);
        assert!(head.starts_with("HTTP/1.1 206") && head.contains("Content-Range: bytes 100-199/300000"), "{head}");
        assert_eq!(&part[i + 4..], &data[100..200]);
        assert_eq!(share.downloads(), 1);
        assert!(share.is_running() && share.remaining() > Duration::from_secs(60));

        drop(share);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(TcpStream::connect(&addr).await.is_err());
    }
}
