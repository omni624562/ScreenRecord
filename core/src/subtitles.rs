//! 字幕：自動產生（FFmpeg 8 以上的 whisper 濾鏡，用 whisper.cpp 在這台電腦上辨識語音，不用上傳），
//! 或匯入 SRT 字幕檔。結果轉成繁體中文（台灣用詞），在剪輯視窗變成一段一段的文字標註，可以修改後燒進影片。
//!
//! FFmpeg 不一定有 whisper 濾鏡（要編譯時開啟；程式自動下載的 essentials 版本沒有），所以先用 `ffmpeg -filters` 確認，
//! 沒有時另外下載同一版、有 whisper 的 full 版本，只用來辨識語音。
//! 語音辨識用的 FFmpeg 與語音模型（ggml 格式）第一次使用時下載到資料夾\whisper。

use crate::{tr, trf};
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
    /// whisper.cpp 官方公布的 SHA-1（models/README.md）：下載或自己選的檔案要一樣才用（被換掉、損壞的不用）
    pub sha1: &'static str,
    /// 說明（中文、英文）
    pub labels: [&'static str; 2],
    /// 短的名稱（中文、英文）
    pub short: [&'static str; 2],
    /// 能翻成英文（large-v3-turbo 訓練時移除了翻譯）
    pub translates: bool,
}

impl Model {
    /// 說明（依介面語言）
    pub fn label(&self) -> &'static str {
        crate::tr!(self.labels[0], self.labels[1])
    }

    /// 短的名稱（依介面語言）
    pub fn short(&self) -> &'static str {
        crate::tr!(self.short[0], self.short[1])
    }
}

pub const MODELS: [Model; 3] = [
    Model {
        id: "base",
        file: "ggml-base.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
        mb: 148,
        sha1: "465707469ff3a37a2b9b8d8f89f2f99de7299dac",
        labels: ["標準（約 150 MB，較快）", "Standard (about 150 MB, faster)"],
        short: ["標準（較快）", "Standard (faster)"],
        translates: true,
    },
    Model {
        id: "small",
        file: "ggml-small.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin",
        mb: 488,
        sha1: "55356645c2b361a969dfd0ef2c5a50d530afd8d5",
        labels: ["精準（約 490 MB，較慢）", "Accurate (about 490 MB, slower)"],
        short: ["精準（較慢）", "Accurate (slower)"],
        translates: true,
    },
    Model {
        id: "turbo",
        file: "ggml-large-v3-turbo-q5_0.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin",
        mb: 574,
        sha1: "e050f7970618a659205450ad97eb95a18d69c9ee",
        labels: ["最精準（約 570 MB，最慢；中文建議）", "Most accurate (about 570 MB, slowest; best for Chinese)"],
        short: ["最精準（最慢）", "Most accurate (slowest)"],
        translates: false,
    },
];

/// 翻成英文用的模型：選的模型能翻譯就用它，否則用已經下載好、能翻譯的（精準優先）
pub fn translation_model(m: &Model) -> Option<Model> {
    if m.translates {
        return Some(*m);
    }
    MODELS.iter().rev().find(|o| o.translates && model_ready(o)).copied()
}

/// 辨識的語言：(代碼, 中文名稱, 英文名稱)
pub const LANGUAGES: [(&str, &str, &str); 3] = [("zh", "中文", "Chinese"), ("en", "英文", "English"), ("auto", "自動偵測", "Detect automatically")];

/// 辨識語言的名稱（依介面語言）
pub fn language_name(i: usize) -> &'static str {
    let l = LANGUAGES[i.min(LANGUAGES.len() - 1)];
    crate::tr!(l.1, l.2)
}

/// 語音模型與語音辨識用的 FFmpeg 可以放的資料夾：先找 ScreenRecorder.exe 旁邊的 whisper（整個資料夾帶著走也能用），
/// 再找 %LOCALAPPDATA%\ScreenRecorder\whisper
pub fn model_dirs() -> Vec<PathBuf> {
    let mut v = vec![crate::paths::app_dir().join("whisper"), crate::paths::data_dir().join("whisper")];
    v.dedup();
    v
}

/// 下載、安裝時放的資料夾：exe 旁邊的 whisper 能寫入就放那裡，否則（例如放在 Program Files）放 %LOCALAPPDATA%
pub fn install_dir() -> PathBuf {
    let dirs = model_dirs();
    for dir in &dirs {
        let probe = dir.join(format!(".write-test-{}", std::process::id()));
        if std::fs::create_dir_all(dir).is_ok() && std::fs::write(&probe, "").is_ok() {
            let _ = std::fs::remove_file(&probe);
            return dir.clone();
        }
    }
    dirs.last().cloned().unwrap_or_default()
}

/// 第一個有這個檔案的資料夾裡的位置；都沒有時是安裝的位置（不建立資料夾）
fn find_in_dirs(name: &str, ok: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    model_dirs().into_iter().map(|d| d.join(name)).find(|p| ok(p))
}

/// 語音模型的位置：已經有的那一個（exe 旁邊優先），沒有時是要安裝的位置
pub fn model_path(m: &Model) -> PathBuf {
    find_in_dirs(m.file, |p| model_size_ok(m, p) && magic_ok(p)).unwrap_or_else(|| model_install_path(m))
}

/// 語音模型要安裝到哪裡
pub fn model_install_path(m: &Model) -> PathBuf {
    install_dir().join(m.file)
}

/// 辨識時 FFmpeg 在模型所在的資料夾裡執行
fn model_dir_of(m: &Model) -> PathBuf {
    model_path(m).parent().map(Path::to_path_buf).unwrap_or_else(install_dir)
}

/// whisper.cpp 的模型檔開頭是 "lmgg"（0x67676d6c，little-endian）
/// 是 whisper.cpp 的模型檔（選擇已下載的檔案時檢查）
pub fn is_model_file(path: &Path) -> bool {
    magic_ok(path)
}

fn magic_ok(path: &Path) -> bool {
    use std::io::Read;
    let mut b = [0u8; 4];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut b)).is_ok() && &b == b"lmgg"
}

/// 模型已經下載好
pub fn model_ready(m: &Model) -> bool {
    let p = model_path(m);
    model_size_ok(m, &p) && magic_ok(&p)
}

/// 檔案大小合理（下載中斷的檔案會比較小）
pub fn model_size_ok(m: &Model, path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|md| md.len() > m.mb as u64 * 900_000)
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// 檔案的 SHA-1 和官方公布的一樣
pub fn model_hash_ok(m: &Model, path: &Path) -> bool {
    use sha1::Digest;
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut h = sha1::Sha1::new();
    std::io::copy(&mut f, &mut h).is_ok() && hex(&h.finalize()) == m.sha1
}

/// 下載模型（會等到下載完成；progress：千分比；cancel 設成 true 就停止）
pub fn download_model(m: &Model, progress: &AtomicU32, cancel: &AtomicBool) -> Result<(), String> {
    use std::io::{Read, Write};
    let dir = install_dir();
    std::fs::create_dir_all(&dir).map_err(|e| trf!("無法建立資料夾：{e}", "Couldn't create the folder: {e}"))?;
    let part = dir.join(format!("{}.part", m.file));
    let (len, mut reader) =
        crate::http::download(m.url, std::time::Duration::from_secs(20), std::time::Duration::from_secs(60)).map_err(|e| trf!("下載語音模型失敗：{e}", "Couldn't download the speech model: {e}"))?;
    let total = len.unwrap_or(m.mb as u64 * 1_000_000);
    let mut out = std::fs::File::create(&part).map_err(|e| trf!("無法寫入檔案：{e}", "Couldn't write the file: {e}"))?;
    let mut buf = vec![0u8; 256 * 1024];
    let mut got = 0u64;
    let mut hash = <sha1::Sha1 as sha1::Digest>::new();
    loop {
        if cancel.load(Ordering::Relaxed) {
            drop(out);
            let _ = std::fs::remove_file(&part);
            return Err("已取消下載".into());
        }
        let n = reader.read(&mut buf).map_err(|e| trf!("下載中斷：{}", "Download interrupted: {}", crate::http::describe_io(&e)))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| trf!("無法寫入檔案：{e}", "Couldn't write the file: {e}"))?;
        sha1::Digest::update(&mut hash, &buf[..n]);
        got += n as u64;
        progress.store(((got * 1000) / total.max(1)).min(1000) as u32, Ordering::Relaxed);
    }
    drop(out);
    if !magic_ok(&part) || got < m.mb as u64 * 900_000 {
        let _ = std::fs::remove_file(&part);
        return Err(tr!("下載的語音模型不完整，請再試一次", "The downloaded speech model is incomplete. Please try again.").into());
    }
    let digest = hex(&sha1::Digest::finalize(hash));
    if digest != m.sha1 {
        let _ = std::fs::remove_file(&part);
        crate::warn!("[字幕] 語音模型 {} 檢查碼不符：{digest}", m.file);
        return Err(
            tr!("下載的語音模型和官方發佈的不一樣（可能下載時損壞），請再試一次", "The downloaded speech model doesn't match the official one (it may have been damaged). Please try again.").into()
        );
    }
    std::fs::rename(&part, dir.join(m.file)).map_err(|e| trf!("無法儲存語音模型：{e}", "Couldn't save the speech model: {e}"))?;
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

/// 語音辨識用的 FFmpeg（目前用的 FFmpeg 沒有 whisper 時才下載），和語音模型放在同一個資料夾。
/// 和主要的 FFmpeg 一樣：程式放在「下載」或暫存資料夾時，程式資料夾裡的要是本程式裝的才用（錄完會自動執行它）
pub fn speech_ffmpeg_path() -> PathBuf {
    find_in_dirs(FFMPEG_NAME, |p| p.is_file() && crate::ffmpeg::trusted(p)).unwrap_or_else(speech_ffmpeg_install_path)
}

const FFMPEG_NAME: &str = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };

/// 語音辨識用的 FFmpeg 要安裝到哪裡
pub fn speech_ffmpeg_install_path() -> PathBuf {
    install_dir().join(FFMPEG_NAME)
}

/// 已經下載了語音辨識用的 FFmpeg（只看檔案在不在，介面提示用）
pub fn speech_ffmpeg_downloaded() -> bool {
    let p = speech_ffmpeg_path();
    p.is_file() && crate::ffmpeg::trusted(&p)
}

/// 用哪個 FFmpeg 辨識語音：目前用的有 whisper 就用它，否則用下載的那個；都不行時是 None（要先下載）
pub async fn find_speech_ffmpeg(main: &Path) -> Option<PathBuf> {
    find_speech_ffmpeg_in(main, &speech_ffmpeg_path()).await
}

async fn find_speech_ffmpeg_in(main: &Path, downloaded: &Path) -> Option<PathBuf> {
    if has_whisper(main).await {
        return Some(main.to_path_buf());
    }
    // 找不到可以用的時 speech_ffmpeg_path 回傳要安裝的位置，那裡可能有來路不明的檔案：一樣要檢查
    (downloaded.is_file() && crate::ffmpeg::trusted(downloaded) && has_whisper(downloaded).await).then(|| downloaded.to_path_buf())
}

/// 下載語音辨識用的 FFmpeg（progress：千分比；cancel 設成 true 就停止）
pub async fn download_speech_ffmpeg(progress: Arc<AtomicU32>, cancel: Arc<AtomicBool>) -> Result<PathBuf, String> {
    if !cfg!(windows) {
        return Err(tr!("請改用有 whisper（語音辨識）的 FFmpeg", "Use an FFmpeg build with whisper (speech recognition)").into());
    }
    let target = speech_ffmpeg_install_path();
    crate::info!("[字幕] 下載語音辨識用的 FFmpeg（{} full）", crate::downloader::FFMPEG_VERSION);
    let deps = crate::downloader::Deps {
        urls: crate::downloader::speech_ffmpeg_urls(),
        sha256: crate::downloader::SPEECH_FFMPEG_SHA256.into(),
        target: Box::new(move || Ok(target.clone())),
        ..Default::default()
    };
    let path = tokio::task::spawn_blocking(move || crate::downloader::install_wait(deps, &progress, &cancel)).await.map_err(|e| e.to_string())??;
    if !has_whisper(&path).await {
        let _ = std::fs::remove_file(&path);
        return Err(tr!("下載的 FFmpeg 不能辨識語音", "The downloaded FFmpeg can't recognize speech").into());
    }
    Ok(path)
}

/// 辨識語音的參數。模型與輸出都用檔名（FFmpeg 在模型的資料夾裡執行），
/// 濾鏡參數裡就不會有「C:」的冒號與反斜線要跳脫。track：用第幾條音軌（分軌錄影用麥克風那條，沒有背景音樂、系統音效）
/// translate：翻成英文（whisper 的翻譯，FFmpeg 9 以上）
pub fn whisper_args(input: &str, model_file: &str, out_file: &str, language: &str, track: usize, translate: bool) -> Vec<String> {
    let mut a: Vec<String> = ["-hide_banner", "-nostats", "-loglevel", "error", "-progress", "pipe:1", "-i"].iter().map(|s| s.to_string()).collect();
    a.push(input.into());
    a.extend(["-vn".to_string(), "-map".into(), format!("0:a:{track}"), "-af".into()]);
    let tr = if translate { ":translate=1" } else { "" };
    a.push(format!("whisper=model={model_file}:language={language}{tr}:queue=10:format=srt:destination={out_file}"));
    a.extend(["-f", "null", "-"].iter().map(|s| s.to_string()));
    a
}

/// 辨識整支影片的語音（progress：千分比；cancel 設成 true 就停止）
pub async fn transcribe(ffmpeg: &Path, input: &str, duration: f64, model: &Model, language: &str, progress: Arc<AtomicU32>, cancel: Arc<AtomicBool>) -> Result<Vec<Cue>, String> {
    transcribe_in(&model_dir_of(model), ffmpeg, input, duration, model, language, false, progress, cancel).await
}

/// 辨識語音並翻成英文（FFmpeg 9 以上的 whisper 才有翻譯）
pub async fn translate(ffmpeg: &Path, input: &str, duration: f64, model: &Model, language: &str, progress: Arc<AtomicU32>, cancel: Arc<AtomicBool>) -> Result<Vec<Cue>, String> {
    let Some(model) = translation_model(model) else {
        return Err(tr!(
            "「最精準」模型不能翻成英文，請先到「設定 → 外部元件」下載「精準」或「標準」模型",
            "The \"Most accurate\" model can't translate to English. Download the \"Accurate\" or \"Standard\" model in Settings → Components first"
        )
        .to_string());
    };
    let model = &model;
    transcribe_in(&model_dir_of(model), ffmpeg, input, duration, model, language, true, progress, cancel).await.map_err(|e| {
        if e.contains("translate") {
            tr!("這個 FFmpeg 不支援翻成英文，請更新到 FFmpeg 9 以上", "This FFmpeg can't translate to English. Update to FFmpeg 9 or later").to_string()
        } else {
            e
        }
    })
}

/// 中英對照：每一句英文放到時間重疊最多的那句原文下面
pub fn pair(orig: &[Cue], en: &[Cue]) -> Vec<Cue> {
    let mut extra: Vec<Vec<&str>> = vec![vec![]; orig.len()];
    for e in en {
        let best = orig.iter().enumerate().map(|(i, o)| (i, (o.end.min(e.end) - o.start.max(e.start)).max(0.0))).filter(|(_, ov)| *ov > 0.0).max_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((i, _)) = best {
            extra[i].push(e.text.trim());
        }
    }
    orig.iter()
        .zip(extra)
        .map(|(o, en)| {
            let en = en.join(" ").replace('\n', " ");
            // 原文自動換行的那一行接回來（中文直接接、英文中間空一格），一行原文、一行英文
            let joint = if o.text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) { "" } else { " " };
            Cue { text: if en.is_empty() { o.text.clone() } else { format!("{}\n{}", o.text.replace('\n', joint), en) }, ..o.clone() }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
async fn transcribe_in(
    dir: &Path,
    ffmpeg: &Path,
    input: &str,
    duration: f64,
    model: &Model,
    language: &str,
    translate: bool,
    progress: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
) -> Result<Vec<Cue>, String> {
    let dir = dir.to_path_buf();
    let out_name = format!("job-{}-{}.srt", std::process::id(), crate::paths::now_ms());
    let out = dir.join(&out_name);
    // 分軌錄影（混音、系統聲音、麥克風）：只聽麥克風
    let head = crate::process::run(ffmpeg, &["-hide_banner", "-i", input], std::time::Duration::from_secs(15)).await;
    let track = if crate::args::parse_media_info(&head.stderr).audio_tracks >= 3 { 2 } else { 0 };
    let mut child = crate::process::command(ffmpeg)
        .args(whisper_args(input, model.file, &out_name, language, track, translate))
        .current_dir(&dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| trf!("無法執行 FFmpeg：{e}", "Couldn't run FFmpeg: {e}"))?;
    let stdout = child.stdout.take().ok_or(tr!("無法讀取 FFmpeg 的進度", "Couldn't read FFmpeg's progress"))?;
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
    // 容錯讀取：語音辨識偶爾把一個中文字切成兩半，整份讀成空的就什麼都沒了
    let text = std::fs::read(&out).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    let _ = std::fs::remove_file(&out);
    if canceled {
        return Err("已取消".into());
    }
    if !status.success() {
        return Err(trf!("語音辨識失敗：{}", "Speech recognition failed: {}", if last_err.is_empty() { tr!("FFmpeg 結束時發生錯誤", "FFmpeg exited with an error").to_string() } else { last_err }));
    }
    progress.store(1000, Ordering::Relaxed);
    Ok(tidy(parse_srt(&text)).into_iter().filter(|c| !is_hallucination(&c.text)).collect())
}

/// 轉成繁體中文（台灣用詞）、去掉空白的段落與辨識不出來的標記
pub fn tidy(cues: Vec<Cue>) -> Vec<Cue> {
    cues.into_iter().map(|c| Cue { text: traditional(c.text.trim()), ..c }).filter(|c| !c.text.is_empty() && !is_noise(&c.text) && c.end > c.start).collect()
}

/// whisper 在沒有說話（安靜、只有音樂）的地方常「聽」出訓練資料裡 YouTube 影片結尾的固定句子，
/// 例如「請不吝點讚 訂閱 轉發 打賞支持明鏡與點點欄目」「字幕由 Amara.org 社群提供」。
/// 只認一定是這種句子的特徵（打賞、Amara.org、明鏡與點點…），一般的「謝謝觀看」不算，免得刪到真的講的話
fn is_hallucination(t: &str) -> bool {
    let s: String = t.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
    const ANY: [&str; 12] =
        ["打賞", "明鏡與點點", "明鏡需要您的支持", "點點欄目", "amaraorg", "字幕志願者", "優優獨播劇場", "yoyotelevisionseries", "字幕由amara", "請不吝點讚", "請不吝點贊", "訂閱我的頻道"];
    if ANY.iter().any(|k| s.contains(k)) {
        return true;
    }
    // 「點讚」「訂閱」「轉發」同時出現兩個以上
    ["點讚", "點贊", "訂閱", "轉發", "分享"].iter().filter(|k| s.contains(*k)).count() >= 2 || s.contains("likeandsubscribe") || (s.contains("subscribe") && s.contains("channel"))
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
    ("雙擊", "按兩下"),
    ("單擊", "按一下"),
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
    // VTT 可以省略小時（「01:02.345」）
    let (h, m, sec) = match p.as_slice() {
        [h, m, sec] => (*h, *m, *sec),
        [m, sec] => ("0", *m, *sec),
        _ => return None,
    };
    let v = h.trim().parse::<f64>().ok()? * 3600.0 + m.trim().parse::<f64>().ok()? * 60.0 + sec.trim().parse::<f64>().ok()? + ms.trim().parse::<f64>().ok()? / 10f64.powi(ms.trim().len() as i32);
    Some(v)
}

fn fmt_time(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    format!("{:02}:{:02}:{:02},{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000)
}

/// 讀 SRT 或 WebVTT（編號從 0 或 1 開始都可以、時間可以用逗號或句點、Windows 換行也可以；
/// VTT 的檔頭、NOTE / STYLE 區塊、位置設定與 <v 說話者>、<b> 等標記會略過）
pub fn parse_srt(s: &str) -> Vec<Cue> {
    let s = s.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let vtt = s.trim_start().starts_with("WEBVTT");
    let mut out = Vec::new();
    for block in s.split("\n\n") {
        let lines: Vec<&str> = block.lines().filter(|l| !l.trim().is_empty()).collect();
        if vtt && lines.first().is_some_and(|l| l.starts_with("NOTE") || l.starts_with("STYLE") || l.starts_with("REGION")) {
            continue;
        }
        let Some(i) = lines.iter().position(|l| l.contains("-->")) else { continue };
        let Some((a, b)) = lines[i].split_once("-->") else { continue };
        let (Some(start), Some(end)) = (parse_time(a), parse_time(b.split_whitespace().next().unwrap_or(""))) else { continue };
        let text = lines[i + 1..].join("\n");
        let text = if vtt { strip_tags(&text) } else { text };
        out.push(Cue { start, end, text });
    }
    out
}

/// 去掉 VTT 的標記（<v 說話者>、<b>、<00:00:01.000>…），還原 &amp; 等
fn strip_tags(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut depth = false;
    for c in t.chars() {
        match c {
            '<' => depth = true,
            '>' if depth => depth = false,
            _ if !depth => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<").replace("&gt;", ">").replace("&nbsp;", " ").replace("&amp;", "&")
}

/// 存成 WebVTT（網頁播放器、Teams、Stream 用）
pub fn to_vtt(cues: &[Cue]) -> String {
    let t = |x: f64| fmt_time(x).replace(',', ".");
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let body: Vec<String> = cues.iter().map(|c| format!("{} --> {}\n{}\n", t(c.start), t(c.end), esc(&c.text))).collect();
    format!("WEBVTT\n\n{}", body.join("\n"))
}

/// 在字幕裡找說過的話（不分大小寫；多行的字幕接成一行）
pub fn search(srt: &str, query: &str) -> Vec<crate::types::SpeechHit> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return vec![];
    }
    parse_srt(srt)
        .into_iter()
        .map(|c| (c.start, c.text.lines().map(str::trim).collect::<Vec<_>>().join(" ")))
        .filter(|(_, t)| t.to_lowercase().contains(&q))
        .map(|(start, text)| crate::types::SpeechHit { start, text })
        .collect()
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

fn mostly_ascii(t: &str) -> bool {
    t.chars().filter(|c| c.is_ascii()).count() * 2 > t.chars().count()
}

/// 字幕放進影片前斷行：每一行（中英對照是原文一行、英文一行）太長時各自斷成兩行；英文一行最多 42 個字母、中文 22 個字
pub fn wrap_lines(text: &str) -> String {
    text.lines().filter(|l| !l.trim().is_empty()).map(|l| wrap(l.trim(), if mostly_ascii(l) { 42 } else { 22 })).collect::<Vec<_>>().join("\n")
}

/// 字幕在清單裡用一行顯示：自動斷行的接回來；中英對照的兩種語言中間用「 / 」隔開（改完用 unflat 換回來）
pub fn flat(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let mut out = String::new();
    for (i, l) in lines.iter().enumerate() {
        if i > 0 {
            let prev = lines[i - 1];
            out.push_str(if mostly_ascii(prev) != mostly_ascii(l) {
                " / "
            } else if mostly_ascii(l) {
                " "
            } else {
                ""
            });
        }
        out.push_str(l);
    }
    out
}

/// flat 的反過來：「 / 」換成換行，每一行再自動斷行
pub fn unflat(text: &str) -> String {
    wrap_lines(&text.split(" / ").collect::<Vec<_>>().join("\n"))
}

/// 在影片旁邊存一份 SRT（同名；已經有就加 _2）
pub fn save_next_to(video: &str, cues: &[Cue]) -> Result<PathBuf, String> {
    save_next_to_as(video, cues, ".srt")
}

/// 英文字幕存成「同名.en.srt」（播放器、YouTube 認得語言）
pub fn save_english_next_to(video: &str, cues: &[Cue]) -> Result<PathBuf, String> {
    save_next_to_as(video, cues, ".en.srt")
}

fn save_next_to_as(video: &str, cues: &[Cue], ext: &str) -> Result<PathBuf, String> {
    let p = Path::new(video);
    let (Some(dir), Some(stem)) = (p.parent(), p.file_stem()) else { return Err(tr!("檔名不正確", "Invalid file name").into()) };
    let out = crate::paths::unique_path(dir, &stem.to_string_lossy(), ext);
    // 加上 BOM：Windows 的記事本與舊的播放器才不會變成亂碼
    std::fs::write(&out, format!("\u{feff}{}", to_srt(cues))).map_err(|e| trf!("無法儲存字幕檔：{e}", "Couldn't save the subtitle file: {e}"))?;
    Ok(out)
}

/// 選一個 SRT 字幕檔
pub fn pick_file() -> Result<Option<PathBuf>, String> {
    crate::filepick::pick(
        tr!("選擇字幕檔", "Choose a subtitle file"),
        &tr!([("字幕檔（SRT、VTT）", "*.srt;*.vtt"), ("所有檔案", "*.*")], [("Subtitle files (SRT, VTT)", "*.srt;*.vtt"), ("All files", "*.*")]),
    )
}

/// 讀 SRT 或 VTT 檔（UTF-8；不是 UTF-8 時當作 Big5 以外的編碼無法處理，回報錯誤）
pub fn read_file(path: &Path) -> Result<Vec<Cue>, String> {
    let bytes = std::fs::read(path).map_err(|e| trf!("無法讀取字幕檔：{e}", "Couldn't read the subtitle file: {e}"))?;
    let text = String::from_utf8(bytes).map_err(|_| tr!("字幕檔不是 UTF-8 編碼，請先用記事本另存成 UTF-8", "The subtitle file isn't UTF-8 encoded. Save it as UTF-8 in Notepad first.").to_string())?;
    let cues = tidy(parse_srt(&text));
    if cues.is_empty() {
        return Err(tr!("字幕檔裡沒有可以用的字幕", "The subtitle file has no usable subtitles").into());
    }
    Ok(cues)
}

/// 字幕的樣式（剪輯視窗套用到全部字幕，之後加的字幕也用這個）
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SubStyle {
    /// 字級：0 = 小、1 = 中、2 = 大
    #[serde(default = "mid")]
    pub size: u8,
    /// 文字顏色 #rrggbb
    #[serde(default = "white")]
    pub color: String,
    /// 半透明深色底
    #[serde(default = "yes")]
    pub bg: bool,
    /// 放在畫面上方（否則下方）
    #[serde(default)]
    pub top: bool,
}

fn mid() -> u8 {
    1
}
fn white() -> String {
    "#ffffff".into()
}
fn yes() -> bool {
    true
}

impl Default for SubStyle {
    fn default() -> Self {
        SubStyle { size: 1, color: white(), bg: true, top: false }
    }
}

/// 字幕可選的文字顏色：白、黃、淺藍
pub const SUB_COLORS: [&str; 3] = ["#ffffff", "#ffd84d", "#7fe7ff"];

impl SubStyle {
    /// 字級（像素）：1080p 時小 32、中 40、大 52
    pub fn font_px(&self, video_h: f64) -> f64 {
        let base = [32.0, 40.0, 52.0][self.size.min(2) as usize];
        (base * video_h / 1080.0).round().max(14.0)
    }

    /// 放好的位置（左上角）：水平置中，離上緣或下緣 6%
    pub fn place(&self, w: f64, h: f64, video_w: f64, video_h: f64) -> (f64, f64) {
        let x = ((video_w - w) / 2.0).round().max(0.0);
        let margin = video_h * 0.06;
        let y = if self.top { margin.round() } else { (video_h - h - margin).round().max(0.0) };
        (x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vtt_round_trip_and_tags() {
        let v = "WEBVTT\nKind: captions\n\nNOTE 這是註解\n\nintro\n00:01.500 --> 00:03.000 align:start\n<v 小明>大家好 &amp; 歡迎</v>\n\n00:00:04.000 --> 00:00:05.250\n第二句\n";
        let cues = parse_srt(v);
        assert_eq!(cues.len(), 2);
        assert!((cues[0].start - 1.5).abs() < 1e-9 && (cues[0].end - 3.0).abs() < 1e-9);
        assert_eq!(cues[0].text, "大家好 & 歡迎");
        assert!((cues[1].end - 5.25).abs() < 1e-9);
        let out = to_vtt(&cues);
        assert!(out.starts_with("WEBVTT\n\n00:00:01.500 --> 00:00:03.000\n大家好 &amp; 歡迎\n"), "{out}");
        assert_eq!(parse_srt(&out), cues);
        // SRT 不會去掉 < >
        assert_eq!(parse_srt("1\n00:00:01,000 --> 00:00:02,000\na <b> c\n")[0].text, "a <b> c");
    }

    #[test]
    fn exe_folder_comes_first() {
        let dirs = model_dirs();
        assert_eq!(dirs[0], crate::paths::app_dir().join("whisper"));
        assert!(dirs.contains(&crate::paths::data_dir().join("whisper")));
        // 安裝的位置一定是其中之一；還沒有檔案時，找的位置就是安裝的位置
        let inst = install_dir();
        assert!(dirs.contains(&inst), "{inst:?}");
        if !dirs.iter().any(|d| d.join("ggml-small.bin").exists()) {
            assert_eq!(model_path(&MODELS[1]), inst.join("ggml-small.bin"));
        }
    }

    #[test]
    fn turbo_translates_with_another_model() {
        let turbo = MODELS.iter().find(|m| m.id == "turbo").unwrap();
        assert!(!turbo.translates && MODELS.iter().filter(|m| m.translates).count() == 2);
        assert_eq!(translation_model(&MODELS[0]), Some(MODELS[0]));
        // 最精準不能翻譯：用已下載、能翻譯的模型，沒有就 None（要請使用者下載）
        match translation_model(turbo) {
            Some(m) => assert!(m.translates && model_ready(&m)),
            None => assert!(!MODELS.iter().any(|m| m.translates && model_ready(m))),
        }
        let need = crate::components::missing_for_translation(2);
        assert_eq!(need.is_empty(), translation_model(turbo).is_some());
        assert!(crate::components::missing_for_translation(0).is_empty());
    }

    #[test]
    fn subtitle_style_sizes_and_places() {
        let st = SubStyle::default();
        assert_eq!(st.font_px(1080.0), 40.0);
        assert_eq!(SubStyle { size: 2, ..st.clone() }.font_px(720.0), 35.0);
        assert_eq!(SubStyle { size: 0, ..st.clone() }.font_px(200.0), 14.0);
        assert_eq!(st.place(400.0, 60.0, 1920.0, 1080.0), (760.0, 955.0));
        assert_eq!(SubStyle { top: true, ..st.clone() }.place(400.0, 60.0, 1920.0, 1080.0), (760.0, 65.0));
        // 舊的專案沒有這個欄位：用預設
        let s: SubStyle = serde_json::from_str("{}").unwrap();
        assert_eq!(s, SubStyle::default());
    }

    #[test]
    fn drops_whisper_hallucinations() {
        for t in [
            "請不吝點讚 訂閱 轉發 打賞支持明鏡與點點欄目",
            "字幕由 Amara.org 社群提供",
            "請不吝點贊訂閱",
            "優優獨播劇場——YoYo Television Series Exclusive",
            "Please like and subscribe!",
            "Subscribe to my channel",
        ] {
            assert!(is_hallucination(&traditional(t)), "{t}");
        }
        for t in ["謝謝觀看", "接下來點開設定", "把檔案分享給同事", "Thanks for watching", "訂單編號在右上角"] {
            assert!(!is_hallucination(t), "{t}");
        }
    }

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
        assert_eq!(traditional("双击快捷键打开摄像头"), "按兩下快速鍵打開攝影機");
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
        let cues = transcribe_in(&models, &fake, "/videos/a.mp4", 2.0, &MODELS[0], "zh", false, progress.clone(), Arc::new(AtomicBool::new(false))).await.unwrap();
        assert_eq!(cues, vec![Cue { start: 0.0, end: 1.5, text: "測試字幕".into() }]);
        assert_eq!(progress.load(Ordering::Relaxed), 1000);
        // 暫存的 SRT 已刪掉，只剩模型
        assert_eq!(std::fs::read_dir(&models).unwrap().count(), 1);
        // FFmpeg 失敗：回報最後一行錯誤
        std::fs::remove_file(models.join(MODELS[0].file)).unwrap();
        let err = transcribe_in(&models, &fake, "a.mp4", 2.0, &MODELS[0], "zh", false, progress, Arc::new(AtomicBool::new(false))).await.unwrap_err();
        assert!(err.contains("no model ggml-base.bin"), "{err}");
    }

    /// 目前用的 FFmpeg 有 whisper 就用它；沒有時用下載的那個；都沒有時要下載
    #[cfg(unix)]
    #[tokio::test]
    async fn picks_an_ffmpeg_that_can_recognize_speech() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fake = |name: &str, filters: &str| {
            let p = dir.path().join(name);
            std::fs::write(&p, format!("#!/bin/sh\nprintf '{filters}'\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        let plain = fake("essentials", " ... aecho            A->A       Add echoing.\\n");
        let full = fake("full", " ... aecho            A->A       Add echoing.\\n ... whisper          A->A       Transcribe audio using whisper.cpp.\\n");
        assert_eq!(find_speech_ffmpeg_in(&full, &plain).await, Some(full.clone()));
        assert_eq!(find_speech_ffmpeg_in(&plain, &full).await, Some(full.clone()));
        assert_eq!(find_speech_ffmpeg_in(&plain, &dir.path().join("missing")).await, None);
        assert_eq!(find_speech_ffmpeg_in(&plain, &plain).await, None);
    }

    #[test]
    fn whisper_detection_and_args() {
        let list = " ... aecho            A->A       Add echoing to the audio.\n ... whisper          A->A       Transcribe audio using whisper.cpp.\n";
        assert!(filters_have_whisper(list));
        assert!(!filters_have_whisper(" ... aecho  A->A  x\n"));
        let a = whisper_args("C:\\影片\\a.mp4", "ggml-base.bin", "job.srt", "zh", 0, false).join(" ");
        assert!(whisper_args("a.mp4", "m.bin", "j.srt", "zh", 2, false).join(" ").contains("-map 0:a:2"));
        assert!(whisper_args("a.mp4", "m.bin", "j.srt", "zh", 0, true).join(" ").contains("language=zh:translate=1:"));
        assert_eq!(
            a,
            "-hide_banner -nostats -loglevel error -progress pipe:1 -i C:\\影片\\a.mp4 -vn -map 0:a:0 -af whisper=model=ggml-base.bin:language=zh:queue=10:format=srt:destination=job.srt -f null -"
        );
        assert!(MODELS.iter().all(|m| m.url.ends_with(m.file)));
        assert!(MODELS.iter().all(|m| m.sha1.len() == 40));
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("m.bin");
        std::fs::write(&f, "abc").unwrap();
        let m = Model { sha1: "a9993e364706816aba3e25717850c26c9cd0d89d", ..MODELS[0] };
        assert!(model_hash_ok(&m, &f));
        assert!(!model_hash_ok(&MODELS[0], &f));
    }

    #[test]
    fn pair_puts_english_under_the_overlapping_sentence() {
        let c = |a: f64, b: f64, t: &str| Cue { start: a, end: b, text: t.into() };
        let zh = vec![c(0.0, 2.0, "大家好"), c(2.0, 5.0, "今天介紹\n報價單"), c(5.0, 6.0, "謝謝")];
        let en = vec![c(0.1, 1.9, "Hello everyone"), c(2.2, 3.5, "Today I'll show"), c(3.5, 5.2, "the quote form")];
        let p = pair(&zh, &en);
        assert_eq!(p.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(), vec!["大家好\nHello everyone", "今天介紹報價單\nToday I'll show the quote form", "謝謝"]);
        assert_eq!((p[1].start, p[1].end), (2.0, 5.0));
    }

    #[test]
    fn flat_and_unflat_keep_the_two_languages_apart() {
        assert_eq!(flat("今天介紹\n報價單"), "今天介紹報價單");
        assert_eq!(flat("Hello every\nbody"), "Hello every body");
        assert_eq!(flat("大家好\nHello everyone"), "大家好 / Hello everyone");
        assert_eq!(unflat("大家好 / Hello everyone"), "大家好\nHello everyone");
        assert_eq!(unflat("短句"), "短句");
        assert_eq!(wrap_lines("大家好\nHello"), "大家好\nHello");
    }
}
