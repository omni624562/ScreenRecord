//! 字幕：自動產生（FFmpeg 8 以上的 whisper 濾鏡，用 whisper.cpp 在這台電腦上辨識語音，不用上傳），
//! 或匯入 SRT 字幕檔。結果轉成繁體中文（台灣用詞），在剪輯視窗變成一段一段的文字標註，可以修改後燒進影片。
//!
//! FFmpeg 不一定有 whisper 濾鏡（要編譯時打開），所以先用 `ffmpeg -filters` 確認。
//! 語音模型（ggml 格式）第一次使用時下載到資料夾\whisper。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};

/// 一段字幕（秒）
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// 語音模型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Model {
    pub id: &'static str,
    pub file: &'static str,
    pub url: &'static str,
    /// 大約幾 MB
    pub mb: u32,
    /// 說明（中文、英文）
    pub labels: [&'static str; 2],
}

impl Model {
    /// 說明（依介面語言）
    pub fn label(&self) -> &'static str {
        crate::tr!(self.labels[0], self.labels[1])
    }
}

pub const MODELS: [Model; 2] = [
    Model {
        id: "base",
        file: "ggml-base.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
        mb: 148,
        labels: ["標準（約 150 MB，較快）", "Standard (about 150 MB, faster)"],
    },
    Model {
        id: "small",
        file: "ggml-small.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin",
        mb: 488,
        labels: ["精準（約 490 MB，較慢）", "Accurate (about 490 MB, slower)"],
    },
];

/// 辨識的語言：(代碼, 中文名稱, 英文名稱)
pub const LANGUAGES: [(&str, &str, &str); 3] = [("zh", "中文", "Chinese"), ("en", "英文", "English"), ("auto", "自動偵測", "Detect automatically")];

/// 辨識語言的名稱（依介面語言）
pub fn language_name(i: usize) -> &'static str {
    let l = LANGUAGES[i.min(LANGUAGES.len() - 1)];
    crate::tr!(l.1, l.2)
}

pub fn model_dir() -> PathBuf {
    crate::paths::data_dir().join("whisper")
}

pub fn model_path(m: &Model) -> PathBuf {
    model_dir().join(m.file)
}

/// whisper.cpp 的模型檔開頭是 "lmgg"（0x67676d6c，little-endian）
fn magic_ok(path: &Path) -> bool {
    use std::io::Read;
    let mut b = [0u8; 4];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut b)).is_ok() && &b == b"lmgg"
}

/// 模型已經下載好
pub fn model_ready(m: &Model) -> bool {
    let p = model_path(m);
    std::fs::metadata(&p).is_ok_and(|md| md.len() > m.mb as u64 * 900_000) && magic_ok(&p)
}

/// 下載模型（會等到下載完成；progress：千分比；cancel 設成 true 就停止）
pub fn download_model(m: &Model, progress: &AtomicU32, cancel: &AtomicBool) -> Result<(), String> {
    use std::io::{Read, Write};
    let dir = model_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("無法建立資料夾：{e}"))?;
    let part = dir.join(format!("{}.part", m.file));
    let resp = crate::http::agent().redirects(8).build().get(m.url).call().map_err(|e| match e {
        ureq::Error::Status(code, _) => format!("下載語音模型失敗（HTTP {code}）"),
        e => format!("下載語音模型失敗：{e}"),
    })?;
    let total = resp.header("Content-Length").and_then(|v| v.parse::<u64>().ok()).unwrap_or(m.mb as u64 * 1_000_000);
    let mut reader = resp.into_reader();
    let mut out = std::fs::File::create(&part).map_err(|e| format!("無法寫入檔案：{e}"))?;
    let mut buf = vec![0u8; 256 * 1024];
    let mut got = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            drop(out);
            let _ = std::fs::remove_file(&part);
            return Err("已取消下載".into());
        }
        let n = reader.read(&mut buf).map_err(|e| format!("下載中斷：{e}"))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| format!("無法寫入檔案：{e}"))?;
        got += n as u64;
        progress.store(((got * 1000) / total.max(1)).min(1000) as u32, Ordering::Relaxed);
    }
    drop(out);
    if !magic_ok(&part) || got < m.mb as u64 * 900_000 {
        let _ = std::fs::remove_file(&part);
        return Err("下載的語音模型不完整，請再試一次".into());
    }
    std::fs::rename(&part, model_path(m)).map_err(|e| format!("無法儲存語音模型：{e}"))?;
    crate::info!("[字幕] 已下載語音模型 {}（{} MB）", m.file, got / 1_000_000);
    Ok(())
}

/// 這個 FFmpeg 有沒有 whisper 濾鏡
pub async fn has_whisper(ffmpeg: &Path) -> bool {
    let r = crate::process::run(ffmpeg, &["-hide_banner", "-filters"], std::time::Duration::from_secs(10)).await;
    filters_have_whisper(&r.stdout)
}

fn filters_have_whisper(list: &str) -> bool {
    list.lines().any(|l| l.split_whitespace().nth(1) == Some("whisper"))
}

/// 辨識語音的參數。模型與輸出都用檔名（FFmpeg 在模型的資料夾裡執行），
/// 濾鏡參數裡就不會有「C:」的冒號與反斜線要跳脫
pub fn whisper_args(input: &str, model_file: &str, out_file: &str, language: &str) -> Vec<String> {
    let mut a: Vec<String> = ["-hide_banner", "-nostats", "-loglevel", "error", "-progress", "pipe:1", "-i"].iter().map(|s| s.to_string()).collect();
    a.push(input.into());
    a.extend(["-vn", "-map", "0:a:0", "-af"].iter().map(|s| s.to_string()));
    a.push(format!("whisper=model={model_file}:language={language}:queue=10:format=srt:destination={out_file}"));
    a.extend(["-f", "null", "-"].iter().map(|s| s.to_string()));
    a
}

/// 辨識整支影片的語音（progress：千分比；cancel 設成 true 就停止）
pub async fn transcribe(ffmpeg: &Path, input: &str, duration: f64, model: &Model, language: &str, progress: Arc<AtomicU32>, cancel: Arc<AtomicBool>) -> Result<Vec<Cue>, String> {
    transcribe_in(&model_dir(), ffmpeg, input, duration, model, language, progress, cancel).await
}

#[allow(clippy::too_many_arguments)]
async fn transcribe_in(dir: &Path, ffmpeg: &Path, input: &str, duration: f64, model: &Model, language: &str, progress: Arc<AtomicU32>, cancel: Arc<AtomicBool>) -> Result<Vec<Cue>, String> {
    let dir = dir.to_path_buf();
    let out_name = format!("job-{}-{}.srt", std::process::id(), crate::paths::now_ms());
    let out = dir.join(&out_name);
    let mut child = crate::process::command(ffmpeg)
        .args(whisper_args(input, model.file, &out_name, language))
        .current_dir(&dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("無法執行 FFmpeg：{e}"))?;
    let stdout = child.stdout.take().ok_or("無法讀取 FFmpeg 的進度")?;
    let stderr = child.stderr.take();
    let err_task = tokio::spawn(async move {
        let mut s = String::new();
        if let Some(e) = stderr {
            let mut lines = BufReader::new(e).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                s = l;
            }
        }
        s
    });
    let mut lines = BufReader::new(stdout).lines();
    let mut canceled = false;
    loop {
        tokio::select! {
            l = lines.next_line() => match l {
                Ok(Some(l)) => {
                    if let Some(us) = l.strip_prefix("out_time_us=").and_then(|v| v.trim().parse::<f64>().ok()) {
                        if duration > 0.0 {
                            progress.store(((us / 1e6 / duration) * 1000.0).clamp(0.0, 999.0) as u32, Ordering::Relaxed);
                        }
                    }
                }
                _ => break,
            },
            _ = tokio::time::sleep(std::time::Duration::from_millis(300)) => {
                if cancel.load(Ordering::Relaxed) {
                    canceled = true;
                    let _ = child.kill().await;
                    break;
                }
            }
        }
    }
    let status = child.wait().await.map_err(|e| e.to_string())?;
    let last_err = err_task.await.unwrap_or_default();
    let text = std::fs::read_to_string(&out).unwrap_or_default();
    let _ = std::fs::remove_file(&out);
    if canceled {
        return Err("已取消".into());
    }
    if !status.success() {
        return Err(format!("語音辨識失敗：{}", if last_err.is_empty() { "FFmpeg 結束時發生錯誤".to_string() } else { last_err }));
    }
    progress.store(1000, Ordering::Relaxed);
    Ok(tidy(parse_srt(&text)))
}

/// 轉成繁體中文（台灣用詞）、去掉空白的段落與辨識不出來的標記
pub fn tidy(cues: Vec<Cue>) -> Vec<Cue> {
    cues.into_iter().map(|c| Cue { text: traditional(c.text.trim()), ..c }).filter(|c| !c.text.is_empty() && !is_noise(&c.text) && c.end > c.start).collect()
}

/// whisper 在沒有說話時常輸出 [BLANK_AUDIO]、(音樂) 之類的標記
fn is_noise(t: &str) -> bool {
    let t = t.trim();
    (t.starts_with('[') && t.ends_with(']')) || (t.starts_with('(') && t.ends_with(')')) || (t.starts_with('（') && t.ends_with('）'))
}

/// 轉繁體（zhconv 的台灣變體）之後再換成台灣的說法：長的詞在前面（例如「文件夾」比「文件」先換）
const TAIWAN_TERMS: [(&str, &str); 53] = [
    ("文件夾", "資料夾"),
    ("快捷鍵", "快速鍵"),
    ("攝像頭", "攝影機"),
    ("分辨率", "解析度"),
    ("剪切板", "剪貼簿"),
    ("剪貼板", "剪貼簿"),
    ("服務器", "伺服器"),
    ("視頻", "影片"),
    ("音頻", "音訊"),
    ("設置", "設定"),
    ("軟件", "軟體"),
    ("硬件", "硬體"),
    ("網絡", "網路"),
    ("信息", "資訊"),
    ("程序", "程式"),
    ("默認", "預設"),
    ("屏幕", "螢幕"),
    ("全屏", "全螢幕"),
    ("截屏", "截圖"),
    ("錄屏", "錄影"),
    ("鼠標", "滑鼠"),
    ("光標", "游標"),
    ("窗口", "視窗"),
    ("用戶", "使用者"),
    ("質量", "品質"),
    ("打印", "列印"),
    ("菜單", "選單"),
    ("界面", "介面"),
    ("登錄", "登入"),
    ("賬號", "帳號"),
    ("數據", "資料"),
    ("內存", "記憶體"),
    ("優化", "最佳化"),
    ("激活", "啟用"),
    ("卸載", "解除安裝"),
    ("鏈接", "連結"),
    ("博客", "部落格"),
    ("粘貼", "貼上"),
    ("復制", "複製"),
    ("刷新", "重新整理"),
    ("添加", "新增"),
    ("創建", "建立"),
    ("搜索", "搜尋"),
    ("保存", "儲存"),
    ("加載", "載入"),
    ("導出", "匯出"),
    ("導入", "匯入"),
    ("硬盤", "硬碟"),
    ("字體", "字型"),
    ("圖標", "圖示"),
    ("雙擊", "點兩下"),
    ("單擊", "點一下"),
    ("演示", "示範"),
];

pub fn traditional(s: &str) -> String {
    let mut t = zhconv::zhconv(s, zhconv::Variant::ZhTW);
    for (from, to) in TAIWAN_TERMS {
        if t.contains(from) {
            t = t.replace(from, to);
        }
    }
    t
}

/// 「00:01:02,345」→ 秒
fn parse_time(s: &str) -> Option<f64> {
    let s = s.trim().replace('.', ",");
    let (hms, ms) = s.split_once(',').unwrap_or((&s, "0"));
    let p: Vec<&str> = hms.split(':').collect();
    let [h, m, sec] = p.as_slice() else { return None };
    let v = h.trim().parse::<f64>().ok()? * 3600.0 + m.trim().parse::<f64>().ok()? * 60.0 + sec.trim().parse::<f64>().ok()? + ms.trim().parse::<f64>().ok()? / 10f64.powi(ms.trim().len() as i32);
    Some(v)
}

fn fmt_time(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// 讀 SRT（編號從 0 或 1 開始都可以、時間可以用逗號或句點、Windows 換行也可以）
pub fn parse_srt(s: &str) -> Vec<Cue> {
    let s = s.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let mut out = Vec::new();
    for block in s.split("\n\n") {
        let lines: Vec<&str> = block.lines().filter(|l| !l.trim().is_empty()).collect();
        let Some(i) = lines.iter().position(|l| l.contains("-->")) else { continue };
        let Some((a, b)) = lines[i].split_once("-->") else { continue };
        let (Some(start), Some(end)) = (parse_time(a), parse_time(b.split_whitespace().next().unwrap_or(""))) else { continue };
        let text = lines[i + 1..].join("\n");
        out.push(Cue { start, end, text });
    }
    out
}

pub fn to_srt(cues: &[Cue]) -> String {
    cues.iter().enumerate().map(|(i, c)| format!("{}\n{} --> {}\n{}\n", i + 1, fmt_time(c.start), fmt_time(c.end), c.text)).collect::<Vec<_>>().join("\n")
}

/// 太長的一行在中間附近（標點或空白後面）斷成兩行，字幕比較好讀
pub fn wrap(text: &str, max_chars: usize) -> String {
    let t = text.replace('\n', " ");
    let chars: Vec<char> = t.chars().collect();
    if chars.len() <= max_chars {
        return t;
    }
    let mid = chars.len() / 2;
    let breaks = |c: char| matches!(c, ' ' | '，' | '。' | '、' | '？' | '！' | ',' | '.' | '?' | '!' | '；' | ';');
    let at = (0..mid).flat_map(|d| [mid + d, mid - d]).find(|&i| i > 0 && i < chars.len() && breaks(chars[i - 1])).unwrap_or(mid);
    let (a, b): (String, String) = (chars[..at].iter().collect(), chars[at..].iter().collect());
    format!("{}\n{}", a.trim_end(), b.trim_start())
}

/// 在影片旁邊存一份 SRT（同名；已經有就加 _2）
pub fn save_next_to(video: &str, cues: &[Cue]) -> Result<PathBuf, String> {
    let p = Path::new(video);
    let (Some(dir), Some(stem)) = (p.parent(), p.file_stem()) else { return Err("檔名不正確".into()) };
    let out = crate::paths::unique_path(dir, &stem.to_string_lossy(), ".srt");
    // 加上 BOM：Windows 的記事本與舊的播放器才不會變成亂碼
    std::fs::write(&out, format!("\u{feff}{}", to_srt(cues))).map_err(|e| format!("無法儲存字幕檔：{e}"))?;
    Ok(out)
}

/// 選一個 SRT 字幕檔
pub fn pick_file() -> Result<Option<PathBuf>, String> {
    crate::filepick::pick("選擇字幕檔", &[("字幕檔（SRT）", "*.srt"), ("所有檔案", "*.*")])
}

/// 讀 SRT 檔（UTF-8；不是 UTF-8 時當作 Big5 以外的編碼無法處理，回報錯誤）
pub fn read_file(path: &Path) -> Result<Vec<Cue>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("無法讀取字幕檔：{e}"))?;
    let text = String::from_utf8(bytes).map_err(|_| "字幕檔不是 UTF-8 編碼，請先用記事本另存成 UTF-8".to_string())?;
    let cues = tidy(parse_srt(&text));
    if cues.is_empty() {
        return Err("字幕檔裡沒有可以用的字幕".into());
    }
    Ok(cues)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_whisper_srt() {
        // whisper 濾鏡的編號從 0 開始
        let s = "0\n00:00:00,000 --> 00:00:02,500\n 大家好，这是一个软件的演示\n\n1\n00:00:02,500 --> 00:00:04,000\n[BLANK_AUDIO]\n\n2\n00:01:02,345 --> 00:01:05,000\n打开视频\n";
        let c = tidy(parse_srt(s));
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].text, "大家好，這是一個軟體的示範");
        assert_eq!((c[0].start, c[0].end), (0.0, 2.5));
        assert_eq!(c[1].text, "打開影片");
        assert!((c[1].start - 62.345).abs() < 1e-9);
        assert_eq!(traditional("先打开设置，把视频保存到文件夹，再复制到剪切板"), "先打開設定，把影片儲存到資料夾，再複製到剪貼簿");
        assert_eq!(traditional("双击快捷键打开摄像头"), "點兩下快速鍵打開攝影機");
    }

    #[test]
    fn srt_round_trip_and_windows_files() {
        let s = "\u{feff}1\r\n00:00:01.5 --> 00:00:03,000 X1:10\r\nLine one\r\nLine two\r\n\r\n2\r\n00:00:04,000 --> 00:00:05,000\r\nOK\r\n";
        let c = parse_srt(s);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0], Cue { start: 1.5, end: 3.0, text: "Line one\nLine two".into() });
        let out = to_srt(&c);
        assert_eq!(out, "1\n00:00:01,500 --> 00:00:03,000\nLine one\nLine two\n\n2\n00:00:04,000 --> 00:00:05,000\nOK\n");
        assert_eq!(parse_srt(&out), c);
    }

    #[test]
    fn wraps_long_lines() {
        assert_eq!(wrap("短句子", 20), "短句子");
        assert_eq!(wrap("今天要介紹的是，如何在三分鐘內完成設定與匯出", 16), "今天要介紹的是，\n如何在三分鐘內完成設定與匯出");
        let w = wrap("this is a fairly long english subtitle line here", 24);
        assert_eq!(w.lines().count(), 2);
    }

    /// 假的 ffmpeg：在工作目錄寫出 destination 指定的 SRT，並回報進度
    #[cfg(unix)]
    #[tokio::test]
    async fn transcribe_runs_in_the_model_folder() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("ffmpeg");
        let script = r#"#!/bin/sh
for a in "$@"; do case "$a" in whisper=*) dest=$(echo "$a" | sed 's/.*destination=//'); model=$(echo "$a" | sed 's/^whisper=model=//; s/:.*//');; esac; done
[ -f "$model" ] || { echo "no model $model" >&2; exit 1; }
printf '0
00:00:00,000 --> 00:00:01,500
测试字幕

' > "$dest"
echo "out_time_us=1000000"
echo "progress=end"
"#;
        std::fs::write(&fake, script).unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let models = dir.path().join("models");
        std::fs::create_dir_all(&models).unwrap();
        std::fs::write(models.join(MODELS[0].file), b"lmgg").unwrap();
        let progress = Arc::new(AtomicU32::new(0));
        let cues = transcribe_in(&models, &fake, "/videos/a.mp4", 2.0, &MODELS[0], "zh", progress.clone(), Arc::new(AtomicBool::new(false))).await.unwrap();
        assert_eq!(cues, vec![Cue { start: 0.0, end: 1.5, text: "測試字幕".into() }]);
        assert_eq!(progress.load(Ordering::Relaxed), 1000);
        // 暫存的 SRT 已刪掉，只剩模型
        assert_eq!(std::fs::read_dir(&models).unwrap().count(), 1);
        // FFmpeg 失敗：回報最後一行錯誤
        std::fs::remove_file(models.join(MODELS[0].file)).unwrap();
        let err = transcribe_in(&models, &fake, "a.mp4", 2.0, &MODELS[0], "zh", progress, Arc::new(AtomicBool::new(false))).await.unwrap_err();
        assert!(err.contains("no model ggml-base.bin"), "{err}");
    }

    #[test]
    fn whisper_detection_and_args() {
        let list = " ... aecho            A->A       Add echoing to the audio.\n ... whisper          A->A       Transcribe audio using whisper.cpp.\n";
        assert!(filters_have_whisper(list));
        assert!(!filters_have_whisper(" ... aecho  A->A  x\n"));
        let a = whisper_args("C:\\影片\\a.mp4", "ggml-base.bin", "job.srt", "zh").join(" ");
        assert_eq!(
            a,
            "-hide_banner -nostats -loglevel error -progress pipe:1 -i C:\\影片\\a.mp4 -vn -map 0:a:0 -af whisper=model=ggml-base.bin:language=zh:queue=10:format=srt:destination=job.srt -f null -"
        );
        assert!(MODELS.iter().all(|m| m.url.ends_with(m.file)));
    }
}
