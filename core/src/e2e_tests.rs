//! 用真的 FFmpeg 從頭跑一遍：用錄影的參數錄分段（畫面是只錄聲音的卡片、聲音是測試音，
//! 不需要螢幕與音效卡）→ 停止時合併並寫入章節、錄到一半當掉後救回；
//! 再拿一支影片做加速版、GIF、WebP、剪輯（含各種效果）、合併，並用播放器解出畫面。
//!
//! FFmpeg：環境變數 SCREENRECORDER_TEST_FFMPEG，否則用 PATH 上的 ffmpeg；都沒有就略過（印出原因）。
//! 缺少某個編碼器或濾鏡時只略過那一項。

use crate::args::{segment_args, CapturePlan, ENCODERS};
use crate::edit::{AudioFx, ClickZoom, CropInput, EditSpec, FastRange, Overlay, OverlayKind};
use crate::exporter::{ExportCtx, Exporter};
use crate::library::MediaCache;
use crate::types::{AudioConfig, CaptureMethod, Chapter, ExportFormat, ExportState, MediaInfo, MethodPreference, RecordConfig, Rect, SourceConfig};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

fn ffmpeg() -> Option<PathBuf> {
    let p = std::env::var_os("SCREENRECORDER_TEST_FFMPEG").map(PathBuf::from).unwrap_or_else(|| "ffmpeg".into());
    let ok = Command::new(&p).arg("-version").stdin(Stdio::null()).output().is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("略過：找不到 FFmpeg（可設定 SCREENRECORDER_TEST_FFMPEG）");
    }
    ok.then_some(p)
}

/// FFmpeg 有沒有這個編碼器 / 濾鏡（list："-encoders"、"-filters"）
fn has(ff: &Path, list: &str, name: &str) -> bool {
    let out = Command::new(ff).args(["-hide_banner", list]).stdin(Stdio::null()).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    let found = out.lines().any(|l| l.split_whitespace().nth(1) == Some(name));
    if !found {
        eprintln!("略過：FFmpeg 沒有 {name}");
    }
    found
}

fn run_ok(ff: &Path, args: &[&str]) {
    let o = Command::new(ff).args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"]).args(args).output().unwrap();
    assert!(o.status.success(), "ffmpeg {args:?}：{}", String::from_utf8_lossy(&o.stderr));
}

fn record_config() -> RecordConfig {
    RecordConfig {
        source: SourceConfig::All,
        fps: crate::audio_card::FPS,
        scale: 100.0,
        draw_mouse: false,
        max_minutes: 0.0,
        method: MethodPreference::Auto,
        output_dir: String::new(),
        shot_dir: String::new(),
        audio: AudioConfig::default(),
        encoder: None,
        countdown_sec: None,
        hide_ui: None,
        show_clicks: false,
        show_keys: false,
        cursor_halo: false,
        hide_icons: false,
        follow_window: None,
        camera: None,
        audio_only: true,
        window_capture: false,
        separate_audio: false,
        mic_denoise: false,
        quiet: false,
        masks: vec![],
        replay_sec: 0,
        monitor_key: None,
    }
}

/// 用錄影的參數錄一個分段 secs 秒：crash = false 時像按下停止一樣送 q 收尾；true 時直接砍掉（當機、停電）
fn record_segment(ff: &Path, card: &Path, out: &Path, secs: f64, crash: bool) {
    // 錄影時聲音以實際速度從 TCP 進來；這裡用 -re 讓測試音也照實際速度
    record_segment_with(ff, card, out, secs, crash, &record_config(), "sine=frequency=440:sample_rate=48000,aformat=channel_layouts=stereo");
}

fn record_segment_with(ff: &Path, card: &Path, out: &Path, secs: f64, crash: bool, config: &RecordConfig, audio_src: &str) {
    let (w, h) = (crate::audio_card::SIZE.0 as i32, crate::audio_card::SIZE.1 as i32);
    let plan = CapturePlan { rect: Rect { x: 0, y: 0, width: w, height: h }, dda: None, monitors: vec![], out_width: w, out_height: h, card: Some(card.display().to_string()) };
    let audio: Vec<String> = ["-re", "-f", "lavfi", "-i", audio_src].iter().map(|s| s.to_string()).collect();
    let args = segment_args(&plan, config, CaptureMethod::Gdigrab, &ENCODERS[0], &out.display().to_string(), Some(&audio), None).unwrap();
    let mut child = Command::new(ff).args(&args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    // 從 FFmpeg 真的開始寫檔案才開始計時：電腦忙的時候（CI）讀取輸入、準備編碼器要好幾秒，固定等 secs 秒會錄得太短
    wait_until(|| out.metadata().is_ok_and(|m| m.len() > 0), "FFmpeg 沒有開始寫檔案");
    std::thread::sleep(Duration::from_secs_f64(secs));
    let mut stdin = child.stdin.take().unwrap();
    if crash {
        child.kill().unwrap();
    } else {
        // 與錄影相同：送 q 後 stdin 保持開著，等 FFmpeg 自己結束（最多 10 秒，之後強制結束）
        stdin.write_all(b"q").unwrap();
        stdin.flush().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() {
            if std::time::Instant::now() > deadline {
                let _ = child.kill();
                panic!("FFmpeg 收到 q 後沒有結束");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let _ = child.wait();
    drop(stdin);
    assert!(out.metadata().is_ok_and(|m| m.len() > 0), "沒有錄到 {}", out.display());
}

/// 等到 ok() 成立（最多 30 秒）
fn wait_until(ok: impl Fn() -> bool, what: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !ok() {
        assert!(std::time::Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn write_card(dir: &Path) -> PathBuf {
    let card = dir.join("card_src.png");
    crate::audio_card::render("2026-10-11 10:00").unwrap().save_png(&card).unwrap();
    card
}

async fn probe(ff: &Path, path: &Path) -> MediaInfo {
    MediaCache::default().probe(ff, &path.display().to_string()).await.unwrap()
}

/// 分段的聲音長度（秒）：合併時每段在聲音結尾截斷，畫面比聲音長的部分不要
async fn audio_sec(ff: &Path, file: &Path) -> f64 {
    let r = crate::process::run(ff, &crate::args::audio_end_args(&file.display().to_string()), Duration::from_secs(30)).await;
    let us = r.stdout.lines().filter_map(|l| l.strip_prefix("out_time_us=")).filter_map(|v| v.trim().parse::<f64>().ok()).next_back();
    us.expect("讀不到聲音長度") / 1e6
}

fn near(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

fn starts(ch: &[Chapter]) -> Vec<f64> {
    ch.iter().map(|c| (c.start * 10.0).round() / 10.0).collect()
}

#[tokio::test]
async fn recording_segments_merge_with_chapters() {
    let Some(ff) = ffmpeg() else { return };
    if !has(&ff, "-encoders", "libx264") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let card = write_card(dir.path());
    let parts = dir.path().join(".parts").join("2026-10-11_10-00-00");
    std::fs::create_dir_all(&parts).unwrap();
    // 錄一段、暫停、再錄一段
    let segs = [parts.join("seg_000.mp4"), parts.join("seg_001.mp4")];
    record_segment(&ff, &card, &segs[0], 2.5, false);
    record_segment(&ff, &card, &segs[1], 2.0, false);
    // 分段的長度（畫面與聲音較長的那個）與聲音的長度。測試的畫面是一張圖，常比測試音多出零點幾秒
    // （CI 忙的時候更多），合併時那一段會在聲音結尾截斷
    let (mut expect, mut audio) = (0.0, 0.0);
    for s in &segs {
        let i = probe(&ff, s).await;
        assert_eq!(i.has_audio, Some(true));
        assert_eq!((i.width, i.height), (Some(640), Some(360)));
        let d = i.duration_sec.unwrap();
        expect += d;
        audio += audio_sec(&ff, s).await.min(d);
    }
    assert!(audio > 3.5, "分段太短：聲音 {audio} 秒");

    let out = dir.path().join("Rec_2026-10-11_10-00-00.mp4");
    let chapters = crate::chapters::from_markers(&[1.0, 3.0], expect);
    let files: Vec<String> = segs.iter().map(|s| s.display().to_string()).collect();
    let duration = crate::recorder::merge_segments(&ff, &files, &parts, &out, true, Some(crate::chapters::ffmetadata(&chapters, expect))).await.unwrap().unwrap();
    // 兩段都要在：至少有各段的聲音那麼長，最多是分段加起來的長度（舊版 FFmpeg 不截斷）
    assert!(duration >= audio - 0.3 && duration <= expect + 0.3, "合併後 {duration} 秒，各段聲音加起來 {audio} 秒，分段加起來 {expect} 秒");

    let info = probe(&ff, &out).await;
    assert_eq!(info.has_audio, Some(true));
    assert_eq!(info.chapters.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(), ["開頭", "標記 1", "標記 2"]);
    assert_eq!(starts(&info.chapters), [0.0, 1.0, 3.0]);
    // 停止後的收尾：分段資料夾刪掉
    crate::recorder::cleanup_parts(Some(&parts));
    assert!(!parts.exists());
}

#[tokio::test]
async fn crashed_recording_is_recovered() {
    let Some(ff) = ffmpeg() else { return };
    if !has(&ff, "-encoders", "libx264") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let card = write_card(dir.path());
    let parts = dir.path().join(".parts").join("2026-10-11_11-00-00");
    std::fs::create_dir_all(&parts).unwrap();
    record_segment(&ff, &card, &parts.join("seg_000.mp4"), 2.0, false);
    // 第二段錄到一半當掉：檔案沒有正常收尾
    record_segment(&ff, &card, &parts.join("seg_001.mp4"), 3.5, true);
    let first = probe(&ff, &parts.join("seg_000.mp4")).await.duration_sec.unwrap();

    let found = crate::recovery::find(dir.path(), None);
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].stamp, "2026-10-11_11-00-00");
    assert_eq!(found[0].files.len(), 2);
    let r = crate::recovery::recover(&ff, &found[0]).await.unwrap();
    let out = dir.path().join("Rec_2026-10-11_11-00-00.mp4");
    assert_eq!(r.path.as_deref(), Some(out.display().to_string().as_str()));
    // 每秒寫入一次：當掉的那段只少最後 1 秒左右（另外扣掉 FFmpeg 啟動的時間，同時跑其他測試時會比較慢）
    assert!(r.video_sec >= first + 1.0 && r.video_sec >= first + 3.5 - 2.5, "救回 {} 秒，第一段 {first} 秒", r.video_sec);
    let info = probe(&ff, &out).await;
    assert_eq!(info.has_audio, Some(true));
    assert!(near(info.duration_sec.unwrap(), r.video_sec, 0.1));
    assert!(!parts.exists(), "救回後分段要刪掉");
    assert!(crate::recovery::find(dir.path(), None).is_empty());
}

/// 一支 6 秒、有聲音、章節在 0 / 2 / 4 秒的影片（像是錄好的錄影）
fn make_source(ff: &Path, dir: &Path) -> PathBuf {
    let meta = dir.join("meta.txt");
    let ch = vec![Chapter { start: 0.0, title: "開頭".into() }, Chapter { start: 2.0, title: "標記 1".into() }, Chapter { start: 4.0, title: "標記 2".into() }];
    std::fs::write(&meta, crate::chapters::ffmetadata(&ch, 6.0)).unwrap();
    let src = dir.join("Rec_2026-10-11_12-00-00.mp4");
    run_ok(
        ff,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=640x360:rate=30:duration=6",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=6",
            "-i",
            &meta.display().to_string(),
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-map_chapters",
            "2",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            &src.display().to_string(),
        ],
    );
    src
}

fn ctx(ff: &Path) -> ExportCtx {
    ExportCtx { ffmpeg: Some(ff.to_path_buf()), encoder: Some(ENCODERS[0]), cache: Arc::default() }
}

async fn finish(ex: &Exporter) -> PathBuf {
    ex.wait().await;
    let s = ex.status().unwrap();
    assert_eq!(s.state, ExportState::Done, "{:?}", s.message);
    PathBuf::from(s.output)
}

#[tokio::test]
async fn exports_keep_chapters_and_lengths() {
    let Some(ff) = ffmpeg() else { return };
    if !has(&ff, "-encoders", "libx264") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = make_source(&ff, dir.path());
    let s = src.display().to_string();
    let c = ctx(&ff);
    let ex = Exporter::new();

    // 加速版 2×：長度減半、章節時間跟著換算、保留聲音
    ex.start(&c, &s, 2.0, true, 0.0, 0.0).await.unwrap();
    let fast = finish(&ex).await;
    assert!(fast.ends_with("Rec_2026-10-11_12-00-00_2x.mp4"));
    let i = probe(&ff, &fast).await;
    assert!(near(i.duration_sec.unwrap(), 3.0, 0.15), "{:?}", i.duration_sec);
    assert_eq!(i.has_audio, Some(true));
    assert_eq!(starts(&i.chapters), [0.0, 1.0, 2.0]);

    // 縮小寬度 + 壓縮到 1 MB 以內（原速）
    ex.start(&c, &s, 1.0, true, 320.0, 1.0).await.unwrap();
    let small = finish(&ex).await;
    // 原速只壓縮：檔名是「_1MB以內」，不是「_1x」
    assert!(small.ends_with("Rec_2026-10-11_12-00-00_1MB以內.mp4"), "{small:?}");
    let i = probe(&ff, &small).await;
    assert_eq!(i.width, Some(320));
    assert!(i.bytes <= 1024 * 1024, "{} bytes", i.bytes);

    // 剪輯：留 0.5～5.5 秒、刪掉 2～3 秒 → 4 秒；在刪掉片段開頭的章節改從下一段開始
    let spec = EditSpec { start: 0.5, end: 5.5, removed: vec![(2.0, 3.0)], ..Default::default() };
    ex.start_cut(&c, &s, &spec, None, None).await.unwrap();
    let cut = finish(&ex).await;
    assert!(cut.ends_with("Rec_2026-10-11_12-00-00_cut.mp4"));
    let i = probe(&ff, &cut).await;
    assert!(near(i.duration_sec.unwrap(), 4.0, 0.15), "{:?}", i.duration_sec);
    assert_eq!(i.has_audio, Some(true));
    assert_eq!(starts(&i.chapters), [0.0, 1.5, 2.5]);

    // 合併兩支：長度相加，章節不沿用（時間會對不上）
    ex.start_merge(&c, &[s.clone(), cut.display().to_string()]).await.unwrap();
    let merged = finish(&ex).await;
    let i = probe(&ff, &merged).await;
    assert!(near(i.duration_sec.unwrap(), 10.0, 0.3), "{:?}", i.duration_sec);
    assert!(i.chapters.is_empty());
}

/// 從 t 秒開始 len 秒的聲音（單聲道 16 位元）裡最大的振幅
fn peak_at(ff: &Path, file: &Path, t: f64, len: f64) -> i32 {
    let o = Command::new(ff)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-ss", &t.to_string(), "-t", &len.to_string(), "-i"])
        .arg(file)
        .args(["-map", "0:a:0", "-ac", "1", "-ar", "48000", "-f", "s16le", "-"])
        .output()
        .unwrap();
    o.stdout.as_chunks::<2>().0.iter().map(|b| (i16::from_le_bytes(*b) as i32).abs()).max().unwrap_or(0)
}

fn mean(v: &[u8]) -> f64 {
    v.iter().map(|&x| x as f64).sum::<f64>() / v.len().max(1) as f64
}

#[tokio::test]
async fn cut_fades_at_joins() {
    let Some(ff) = ffmpeg() else { return };
    if !has(&ff, "-encoders", "libx264") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = make_source(&ff, dir.path());
    let s = src.display().to_string();
    let c = ctx(&ff);
    let ex = Exporter::new();
    // 留 0.5～5.5、刪掉 2～3：接縫在剪好的 1.5 秒；長度不變
    let spec = EditSpec { start: 0.5, end: 5.5, removed: vec![(2.0, 3.0)], fade_cuts: true, ..Default::default() };
    ex.start_cut(&c, &s, &spec, None, None).await.unwrap();
    let cut = finish(&ex).await;
    let i = probe(&ff, &cut).await;
    assert!(near(i.duration_sec.unwrap(), 4.0, 0.15), "{:?}", i.duration_sec);
    // 接縫前後的畫面變暗，離接縫遠的地方照常
    let normal = mean(&gray_frame(&ff, &cut, 0.8, 640, 360));
    let before = mean(&gray_frame(&ff, &cut, 1.47, 640, 360));
    let after = mean(&gray_frame(&ff, &cut, 1.52, 640, 360));
    let later = mean(&gray_frame(&ff, &cut, 2.5, 640, 360));
    assert!(normal > 60.0 && later > 60.0, "{normal} {later}");
    assert!(before < normal * 0.35 && after < later * 0.35, "{before} {after}");
    // 聲音也在接縫降到幾乎沒有
    let loud = peak_at(&ff, &cut, 0.8, 0.02);
    let quiet = peak_at(&ff, &cut, 1.49, 0.02);
    assert!(loud > 1000 && quiet < loud / 5, "{loud} {quiet}");
}

#[tokio::test]
async fn cover_is_saved_and_written_into_the_video() {
    let Some(ff) = ffmpeg() else { return };
    if !has(&ff, "-encoders", "libx264") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = make_source(&ff, dir.path());
    let s = src.display().to_string();
    let before = probe(&ff, &src).await;
    let mtime = std::fs::metadata(&src).unwrap().modified().unwrap();
    let frame = crate::cover::frame_at(&ff, &s, 2.0).await.unwrap();
    assert_eq!((frame.width(), frame.height()), (640, 360));
    let c = crate::cover::Cover { title: "報價單怎麼填".into(), subtitle: "業務部教學".into(), style: crate::cover::CoverStyle::Band };
    let pm = crate::cover::render(&frame, &c).unwrap();
    let saved = crate::cover::save(Some(&ff), &s, &pm, true).await.unwrap();
    assert!(saved.embedded, "{:?}", saved.embed_error);
    assert!(saved.png.ends_with("Rec_2026-10-11_12-00-00_封面.png"));
    let png = tiny_skia::Pixmap::load_png(&saved.png).unwrap();
    assert_eq!((png.width(), png.height()), (1280, 720));
    // 影片照常：長度、聲音、章節不變，多了一張封面圖；修改時間不變
    let after = probe(&ff, &src).await;
    assert!(near(after.duration_sec.unwrap(), before.duration_sec.unwrap(), 0.05));
    assert_eq!((after.width, after.has_audio), (Some(640), Some(true)));
    assert_eq!(starts(&after.chapters), starts(&before.chapters));
    assert_eq!(std::fs::metadata(&src).unwrap().modified().unwrap(), mtime);
    let o = Command::new(&ff).args(["-hide_banner", "-i"]).arg(&src).output().unwrap();
    let info = String::from_utf8_lossy(&o.stderr);
    assert_eq!(info.matches("(attached pic)").count(), 1, "{info}");
    // 再做一次：換掉原本的封面，不會變成兩張；暫存檔不留下
    crate::cover::save(Some(&ff), &s, &pm, true).await.unwrap();
    let o = Command::new(&ff).args(["-hide_banner", "-i"]).arg(&src).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&o.stderr).matches("(attached pic)").count(), 1);
    let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.contains("cover-tmp")).collect();
    assert!(left.is_empty(), "{left:?}");
    // 播放器仍然讀得到畫面（只看主要的畫面，不是封面）
    let f = crate::cover::frame_at(&ff, &s, 1.0).await.unwrap();
    assert_eq!(f.width(), 640);
}

#[tokio::test]
async fn animations_gif_and_webp() {
    let Some(ff) = ffmpeg() else { return };
    if !has(&ff, "-encoders", "libx264") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = make_source(&ff, dir.path());
    let s = src.display().to_string();
    let c = ctx(&ff);
    let ex = Exporter::new();

    ex.start_gif(&c, &s, 2.0, 320.0, 10.0, ExportFormat::Gif).await.unwrap();
    let gif = finish(&ex).await;
    assert!(gif.ends_with("Rec_2026-10-11_12-00-00_2x.gif"));
    let i = probe(&ff, &gif).await;
    assert_eq!(i.width, Some(320));
    assert!(near(i.duration_sec.unwrap(), 3.0, 0.2), "{:?}", i.duration_sec);

    if !has(&ff, "-encoders", "libwebp_anim") {
        return;
    }
    ex.start_gif(&c, &s, 2.0, 320.0, 10.0, ExportFormat::Webp).await.unwrap();
    let webp = finish(&ex).await;
    assert!(webp.ends_with("Rec_2026-10-11_12-00-00_2x.webp"));
    let bytes = std::fs::read(&webp).unwrap();
    assert!(bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP");
    assert!(bytes.len() < std::fs::metadata(&gif).unwrap().len() as usize, "WebP 應該比 GIF 小");
    // 讀回動態 WebP 需要 FFmpeg 8 以後才有的 webp_anim 解碼器（程式附的 FFmpeg 有）
    if !has(&ff, "-decoders", "webp_anim") {
        return;
    }
    // 檔頭沒有長度：讀資訊時另外量
    let i = probe(&ff, &webp).await;
    assert_eq!(i.width, Some(320));
    assert!(near(i.duration_sec.unwrap_or(0.0), 3.0, 0.2), "{:?}", i.duration_sec);
    // 播放器、縮圖、擷取畫面從中間開始解（WebP 不能用一般的跳轉）
    let w = webp.display().to_string();
    let frame = crate::player::decode_frame(&ff, &w, 1.5, 320, 180, false).expect("WebP 解不出畫面");
    assert!(frame.chunks(4).any(|p| p != &frame[..4]), "畫面是單一顏色");
}

#[tokio::test]
async fn player_decodes_frames_at_times() {
    let Some(ff) = ffmpeg() else { return };
    if !has(&ff, "-encoders", "libx264") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = make_source(&ff, dir.path());
    let s = src.display().to_string();
    let a = crate::player::decode_frame(&ff, &s, 0.5, 320, 180, false).unwrap();
    let b = crate::player::decode_frame(&ff, &s, 4.5, 320, 180, false).unwrap();
    assert_eq!(a.len(), 320 * 180 * 4);
    // testsrc2 每張畫面都不一樣
    assert_ne!(a, b);
}

#[tokio::test]
async fn cut_with_every_effect() {
    let Some(ff) = ffmpeg() else { return };
    if !has(&ff, "-encoders", "libx264") || !has(&ff, "-filters", "afftdn") || !has(&ff, "-filters", "loudnorm") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = make_source(&ff, dir.path());
    let s = src.display().to_string();
    let mut png = Vec::new();
    {
        let mut pm = tiny_skia::Pixmap::new(80, 40).unwrap();
        pm.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 200));
        png.extend(pm.encode_png().unwrap());
    }
    let ov = |kind, x, y, png: Option<Vec<u8>>| Overlay { kind, x, y, w: 80.0, h: 40.0, start: 0.5, end: 4.0, png, mask: None, invert: false };
    let spec = EditSpec {
        start: 0.0,
        end: 6.0,
        removed: vec![(1.0, 1.5)],
        crop: Some(CropInput { x: 20.0, y: 10.0, width: 600.0, height: 340.0 }),
        overlays: vec![ov(OverlayKind::Image, 10.0, 10.0, Some(png)), ov(OverlayKind::Blur, 200.0, 100.0, None), ov(OverlayKind::Mosaic, 400.0, 200.0, None)],
        audio: AudioFx { denoise: true, normalize: true, mute: false, tracks: None },
        // 3～5 秒 4 倍速：2 秒變 0.5 秒
        fast: vec![FastRange { start_ms: 3000, end_ms: 5000, speed: 4 }],
        zoom: Some(ClickZoom { factor: 1.5, clicks: vec![[2.0, 0.3, 0.4]] }),
        frame: Some(crate::video_frame::VideoFrame::default()),
        cursor: Some(crate::cursor::CursorFx { size: 2.0, track: vec![[0.0, 0.2, 0.2], [2.0, 0.6, 0.5], [4.0, 0.3, 0.7]] }),
        vertical: None,
        ..Default::default()
    };
    let expect = crate::edit::output_length(&crate::edit::keep_parts(6.0, &spec));
    assert!(near(expect, 4.0, 0.01), "{expect}");
    let ex = Exporter::new();
    ex.start_cut(&ctx(&ff), &s, &spec, None, None).await.unwrap();
    let out = finish(&ex).await;
    let i = probe(&ff, &out).await;
    assert!(near(i.duration_sec.unwrap(), expect, 0.2), "{:?}", i.duration_sec);
    assert_eq!(i.has_audio, Some(true));
    let (w, h) = (i.width.unwrap(), i.height.unwrap());
    assert!(w % 2 == 0 && h % 2 == 0, "{w}×{h}");

    // 不要聲音
    let mute = EditSpec { start: 0.0, end: 5.0, audio: AudioFx { mute: true, ..Default::default() }, ..Default::default() };
    ex.start_cut(&ctx(&ff), &s, &mute, None, None).await.unwrap();
    let out = finish(&ex).await;
    assert_eq!(probe(&ff, &out).await.has_audio, Some(false));
}

#[tokio::test]
async fn idle_parts_are_found() {
    let Some(ff) = ffmpeg() else { return };
    if !has(&ff, "-encoders", "libx264") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // 3 秒有動、有聲音，接著 5 秒畫面停住而且沒有聲音
    let src = dir.path().join("idle.mp4");
    run_ok(
        &ff,
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x180:rate=30:duration=3,tpad=stop_mode=clone:stop_duration=5",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=3,apad=whole_dur=8",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            &src.display().to_string(),
        ],
    );
    let ranges = crate::idle::find(&ff, &src.display().to_string(), 8.0, true).await.unwrap();
    assert_eq!(ranges.len(), 1, "{ranges:?}");
    let (a, b) = ranges[0];
    // 頭尾各留一點
    assert!(a > 3.0 && a < 4.0 && b > 7.0 && b <= 8.0, "{ranges:?}");
}

#[tokio::test]
async fn after_record_cuts_idle_parts_and_compresses() {
    let Some(ff) = ffmpeg() else { return };
    if !has(&ff, "-encoders", "libx264") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // 3 秒有動、有聲音，接著 5 秒畫面停住而且沒有聲音；無損壓縮讓檔案夠大（才需要壓縮）
    let src = dir.path().join("Rec_2026-10-11_13-00-00.mp4");
    run_ok(
        &ff,
        &[
            "-f",
            "lavfi",
            "-i",
            // 雜訊讓前 3 秒很難壓縮；停住的部分重複最後一張（不動）
            "testsrc2=size=640x360:rate=30:duration=3,noise=alls=40:allf=t+u,tpad=stop_mode=clone:stop_duration=5",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000:duration=3,apad=whole_dur=8",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-qp",
            "0",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            &src.display().to_string(),
        ],
    );
    assert!(std::fs::metadata(&src).unwrap().len() > 2 * 1024 * 1024, "測試影片太小");
    let data = tempfile::tempdir().unwrap();
    let app = crate::app::App::with_data_dir(data.path().to_path_buf());
    app.set_ffmpeg_for_test(&ff);
    app.mark_ready();
    app.settings.save(crate::settings::SettingsPatch { ui: Some(serde_json::json!({ "afterIdleCut": true, "afterCompressMb": 1 })), ..Default::default() });
    app.clone().after_record(src.display().to_string()).await;
    assert!(app.after_status().is_none());

    // 剪輯版：只剩有動靜的 3 秒多一點；存了剪輯設定，可以在剪輯視窗再改
    let cut = dir.path().join("Rec_2026-10-11_13-00-00_cut.mp4");
    let i = probe(&ff, &cut).await;
    let d = i.duration_sec.unwrap();
    assert!(d > 3.0 && d < 5.0, "剪輯版 {d} 秒");
    let project = app.projects.for_output(&cut.display().to_string()).expect("沒有存剪輯設定");
    assert_eq!(Path::new(&project.source), src.as_path());
    // 壓縮版（從剪輯版做）：1 MB 以內
    let small: Vec<PathBuf> = std::fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.path()).filter(|p| p != &src && p != &cut).collect();
    assert_eq!(small.len(), 1, "{small:?}");
    assert!(std::fs::metadata(&small[0]).unwrap().len() <= 1024 * 1024);
    assert!(near(probe(&ff, &small[0]).await.duration_sec.unwrap(), d, 0.2));

    // 已經夠小：不再壓縮；沒有設定：什麼都不做
    app.settings.save(crate::settings::SettingsPatch { ui: Some(serde_json::json!({ "afterCompressMb": 100 })), ..Default::default() });
    app.clone().after_record(cut.display().to_string()).await;
    app.settings.save(crate::settings::SettingsPatch { ui: Some(serde_json::json!({})), ..Default::default() });
    app.clone().after_record(src.display().to_string()).await;
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 3);
}

/// 最大音量（dB）
fn max_volume(ff: &Path, file: &Path) -> f64 {
    let o = Command::new(ff).args(["-hide_banner", "-nostdin", "-i"]).arg(file).args(["-map", "0:a:0", "-af", "volumedetect", "-f", "null", "-"]).output().unwrap();
    let err = String::from_utf8_lossy(&o.stderr);
    let line = err.lines().find(|l| l.contains("max_volume:")).unwrap_or_else(|| panic!("{err}"));
    line.split("max_volume:").nth(1).unwrap().trim().trim_end_matches("dB").trim().parse().unwrap()
}

#[tokio::test]
async fn separate_audio_tracks() {
    let Some(ff) = ffmpeg() else { return };
    if !has(&ff, "-encoders", "libx264") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let card = write_card(dir.path());
    let parts = dir.path().join(".parts").join("2026-10-11_14-00-00");
    std::fs::create_dir_all(&parts).unwrap();
    // 聲音管線分軌時送進 6 個聲道：混音、系統聲音（440 Hz）、麥克風（880 Hz）各兩個
    let sys = "0.3*sin(440*2*PI*t)";
    let mic = "0.3*sin(880*2*PI*t)";
    let src = format!("aevalsrc={sys}+{mic}|{sys}+{mic}|{sys}|{sys}|{mic}|{mic}:s=48000:c=5.1");
    let config = RecordConfig { audio: crate::types::AudioConfig { system: true, mic: true, mic_id: String::new(), app: String::new() }, separate_audio: true, ..record_config() };
    assert!(config.separate_tracks());
    let segs = [parts.join("seg_000.mp4"), parts.join("seg_001.mp4")];
    record_segment_with(&ff, &card, &segs[0], 2.0, false, &config, &src);
    record_segment_with(&ff, &card, &segs[1], 2.0, false, &config, &src);
    let out = dir.path().join("Rec_2026-10-11_14-00-00.mp4");
    let files: Vec<String> = segs.iter().map(|s| s.display().to_string()).collect();
    crate::recorder::merge_segments(&ff, &files, &parts, &out, true, None).await.unwrap();
    // 合併後三條音軌都在，第一條（混音）是預設
    let i = probe(&ff, &out).await;
    assert_eq!((i.has_audio, i.audio_tracks), (Some(true), 3));
    let head = Command::new(&ff).args(["-hide_banner", "-i"]).arg(&out).output().unwrap();
    let head = String::from_utf8_lossy(&head.stderr).into_owned();
    for t in ["混音", "系統聲音", "麥克風"] {
        assert!(head.contains(t), "{head}");
    }
    assert!(max_volume(&ff, &out) > -20.0);

    // 剪輯：系統聲音、麥克風都關掉 → 沒有聲音；只留系統聲音 → 有聲音
    let ex = Exporter::new();
    let c = ctx(&ff);
    let s = out.display().to_string();
    let d = i.duration_sec.unwrap();
    let silent = EditSpec { start: 0.0, end: d - 0.5, audio: AudioFx { tracks: Some(crate::edit::TrackMix { system: 0, mic: 0 }), ..Default::default() }, ..Default::default() };
    ex.start_cut(&c, &s, &silent, None, None).await.unwrap();
    let cut = finish(&ex).await;
    assert!(max_volume(&ff, &cut) < -60.0, "{}", max_volume(&ff, &cut));
    assert_eq!(probe(&ff, &cut).await.audio_tracks, 1);
    std::fs::remove_file(&cut).unwrap();
    let sys_only = EditSpec { audio: AudioFx { tracks: Some(crate::edit::TrackMix { system: 100, mic: 0 }), denoise: true, ..Default::default() }, ..silent };
    ex.start_cut(&c, &s, &sys_only, None, None).await.unwrap();
    let cut = finish(&ex).await;
    assert!(max_volume(&ff, &cut) > -20.0);
    // 一般的錄影（一條音軌）選了分軌音量：照常用那一條
    let one = make_source(&ff, dir.path());
    ex.start_cut(&c, &one.display().to_string(), &EditSpec { start: 0.0, end: 5.0, ..sys_only.clone() }, None, None).await.unwrap();
    let cut = finish(&ex).await;
    assert!(max_volume(&ff, &cut) > -20.0);
}

#[tokio::test]
async fn replay_buffer_saves_while_recording() {
    let Some(ff) = ffmpeg() else { return };
    if !has(&ff, "-encoders", "libx264") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let card = write_card(dir.path());
    let parts = dir.path().join(".parts").join("2026-10-11_14-00-00");
    std::fs::create_dir_all(&parts).unwrap();
    let (w, h) = (crate::audio_card::SIZE.0 as i32, crate::audio_card::SIZE.1 as i32);
    let plan = CapturePlan { rect: Rect { x: 0, y: 0, width: w, height: h }, dda: None, monitors: vec![], out_width: w, out_height: h, card: Some(card.display().to_string()) };
    let audio: Vec<String> = ["-re", "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000,aformat=channel_layouts=stereo"].iter().map(|s| s.to_string()).collect();
    let config = RecordConfig { replay_sec: 30, ..record_config() };
    let pattern = parts.join("rp_000_%05d.mp4").display().to_string();
    let mut args = segment_args(&plan, &config, CaptureMethod::Gdigrab, &ENCODERS[0], &pattern, Some(&audio), None).unwrap();
    // 測試不等 10 秒：每 2 秒（每個關鍵畫面）換一個檔案
    let i = args.iter().position(|a| a == "-segment_time").unwrap();
    args[i + 1] = "2".into();
    let mut child = Command::new(&ff).args(&args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    // 從第一個檔案有內容才開始計時（電腦忙的時候 FFmpeg 準備要好幾秒）
    wait_until(|| crate::replay::chunks(&parts).first().is_some_and(|c| c.len > 0), "FFmpeg 沒有開始寫檔案");
    std::thread::sleep(Duration::from_millis(7500));
    // 還在錄的時候存下最近 4 秒：包含正在寫的檔案
    let chunks = crate::replay::chunks(&parts);
    assert!(chunks.len() >= 3, "{chunks:?}");
    let files: Vec<String> = crate::replay::pick(&chunks, std::time::SystemTime::now(), 4).iter().map(|p| p.display().to_string()).collect();
    assert!((2..=4).contains(&files.len()), "{files:?}");
    // 最後一個是有內容的最新檔案（剛開的、只有檔頭的不算）
    let newest = chunks.iter().rev().find(|c| c.len >= 4096).map(|c| c.path.display().to_string());
    assert_eq!(files.last(), newest.as_ref());
    let work = parts.join("save");
    std::fs::create_dir_all(&work).unwrap();
    let out = dir.path().join("Replay_2026-10-11_14-00-07.mp4");
    let dur = crate::recorder::merge_segments(&ff, &files, &work, &out, true, None).await.unwrap().unwrap();
    // 存下的至少 3 秒（最近 4 秒，最新的檔案還在寫）。檔案只在關鍵畫面切開，第一個常常比 2 秒長（錯過 2 秒那個切點就到 4 秒），
    // 所以不用固定的上限，而是看接起來的長度：已經寫完的檔案都要在裡面（接的地方沒有掉），
    // 也不會比現在所有挑到的檔案加起來還長（沒有重複；最新的那個合併之後還在變長）
    let (mut done, mut all) = (0.0, 0.0);
    for (i, f) in files.iter().enumerate() {
        let d = probe(&ff, Path::new(f)).await.duration_sec.unwrap_or(0.0);
        all += d;
        if i + 1 < files.len() {
            done += d;
        }
    }
    assert!(dur >= 3.0 && dur >= done - 0.3 && dur <= all + 0.3, "接起來 {dur} 秒；寫完的檔案 {done} 秒、全部 {all} 秒（{} 個檔案）", files.len());
    let info = probe(&ff, &out).await;
    assert_eq!((info.has_audio, info.width), (Some(true), Some(640)));
    // 測試音約 -21 dB；接起來的地方沒有斷掉（每個檔案的聲音都在）
    let vol = max_volume(&ff, &out);
    assert!(vol > -30.0, "{vol}");
    for f in &files {
        assert!(max_volume(&ff, Path::new(f)) > -30.0, "{f}");
    }
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"q").unwrap();
    stdin.flush().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        assert!(std::time::Instant::now() < deadline, "FFmpeg 收到 q 後沒有結束");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// 一張畫面的灰階像素（w × h）
fn gray_frame(ff: &Path, file: &Path, t: f64, w: usize, h: usize) -> Vec<u8> {
    let o = Command::new(ff)
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-ss", &t.to_string(), "-i"])
        .arg(file)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "gray", "-"])
        .output()
        .unwrap();
    assert_eq!(o.stdout.len(), w * h, "{}", String::from_utf8_lossy(&o.stderr));
    o.stdout
}

#[tokio::test]
async fn smooth_cursor_is_drawn_where_it_should_be() {
    let Some(ff) = ffmpeg() else { return };
    if !has(&ff, "-encoders", "libx264") || !has(&ff, "-filters", "sendcmd") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("Rec_2026-10-11_15-00-00.mp4");
    run_ok(&ff, &["-f", "lavfi", "-i", "color=black:s=640x360:r=30", "-t", "3", "-c:v", "libx264", "-pix_fmt", "yuv420p", &src.display().to_string()]);
    // 0.5 秒時從左上移到右下，之後不動
    let track = vec![[0.0, 0.2, 0.2], [0.5, 0.2, 0.2], [0.53, 0.7, 0.6], [3.0, 0.7, 0.6]];
    let spec = EditSpec { start: 0.0, end: 3.0, cursor: Some(crate::cursor::CursorFx { size: 2.0, track: track.clone() }), ..Default::default() };
    let ex = Exporter::new();
    ex.start_cut(&ctx(&ff), &src.display().to_string(), &spec, None, None).await.unwrap();
    let out = finish(&ex).await;
    let px = |f: &[u8], x: usize, y: usize| f[y * 640 + x];
    // 箭頭的尖端在游標的位置：尖端右下方是白色，左上方是黑色的背景
    for (t, cx, cy) in [(0.3, 128usize, 72usize), (2.5, 448, 216)] {
        let f = gray_frame(&ff, &out, t, 640, 360);
        assert!(px(&f, cx + 2, cy + 7) > 180, "t={t}：{}", px(&f, cx + 2, cy + 7));
        assert!(px(&f, cx - 6, cy - 6) < 40, "t={t}：{}", px(&f, cx - 6, cy - 6));
    }
    // 移動中：平滑地追過去（0.6 秒時還在路上，不在起點也不在終點）
    let s = crate::cursor::smooth(&track, 30.0);
    let (mx, my) = crate::cursor::at(&s, 0.6).unwrap();
    assert!(mx > 0.25 && mx < 0.65, "{mx}");
    let f = gray_frame(&ff, &out, 0.6, 640, 360);
    let (cx, cy) = ((mx * 640.0).round() as usize, (my * 360.0).round() as usize);
    let white = (cx..cx + 6).flat_map(|x| (cy + 2..cy + 12).map(move |y| (x, y))).filter(|&(x, y)| px(&f, x, y) > 180).count();
    assert!(white > 5, "{white}");
    // 暫存的游標圖與指令檔刪掉了
    assert!(!std::fs::read_dir(std::env::temp_dir()).unwrap().flatten().any(|e| e.path().join("cursor.txt").exists() && e.file_name().to_string_lossy().contains(&std::process::id().to_string())));
}

#[tokio::test]
async fn vertical_videos() {
    let Some(ff) = ffmpeg() else { return };
    if !has(&ff, "-encoders", "libx264") || !has(&ff, "-filters", "sendcmd") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    // 黑色畫面，右邊有一個白色方塊
    let src = dir.path().join("Rec_2026-10-11_17-00-00.mp4");
    run_ok(
        &ff,
        &["-f", "lavfi", "-i", "color=black:s=1280x720:r=30,drawbox=x=1100:y=300:w=60:h=60:color=white:t=fill", "-t", "4", "-c:v", "libx264", "-pix_fmt", "yuv420p", &src.display().to_string()],
    );
    let s = src.display().to_string();
    let ex = Exporter::new();
    // 放在中間：720×1280（來源 720p）
    let fit = EditSpec { start: 0.0, end: 4.0, vertical: Some(crate::vertical::Vertical::Fit), ..Default::default() };
    ex.start_cut(&ctx(&ff), &s, &fit, None, None).await.unwrap();
    let out = finish(&ex).await;
    let i = probe(&ff, &out).await;
    assert_eq!((i.width, i.height), (Some(720), Some(1280)));
    // 中間是整個畫面（方塊在右邊），上下是模糊的背景
    let f = gray_frame(&ff, &out, 1.0, 720, 1280);
    let px = |f: &[u8], w: usize, x: usize, y: usize| f[y * w + x];
    let (bx, by) = ((1130.0 * 720.0 / 1280.0) as usize, 640 - 202 + (330.0 * 720.0 / 1280.0) as usize);
    assert!(px(&f, 720, bx, by) > 200, "{}", px(&f, 720, bx, by));
    // 跟著游標：一開始在左邊（看不到方塊），1 秒後游標移到方塊上，鏡頭跟過去
    let track = vec![[0.0, 0.2, 0.5], [1.0, 0.2, 0.5], [1.03, 0.88, 0.45], [4.0, 0.88, 0.45]];
    let follow = EditSpec { start: 0.0, end: 4.0, vertical: Some(crate::vertical::Vertical::Follow { track }), ..Default::default() };
    ex.start_cut(&ctx(&ff), &s, &follow, None, None).await.unwrap();
    let out = finish(&ex).await;
    let i = probe(&ff, &out).await;
    assert_eq!((i.width, i.height), (Some(404), Some(718)));
    let white = |t: f64| gray_frame(&ff, &out, t, 404, 718).iter().filter(|v| **v > 200).count();
    assert_eq!(white(0.5), 0);
    assert!(white(3.5) > 2500, "{}", white(3.5));
}

/// FFmpeg 的主版號（「ffmpeg version 9.0.2-…」→ 9；讀不到時 None）
fn ffmpeg_major(ff: &Path) -> Option<u32> {
    let out = Command::new(ff).args(["-hide_banner", "-version"]).stdin(Stdio::null()).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let v = text.split_whitespace().skip_while(|w| *w != "version").nth(1)?;
    v.trim_start_matches('n').split(['.', '-']).next()?.parse().ok()
}

/// 用真的聲音管線（假的、沒有聲音的來源）錄 secs 秒，和錄影時一樣：聲音管線要看到第一張畫面（showinfo）才開始送聲音，
/// 用來對齊時間。回傳第一張畫面出現花了多久
fn record_with_audio_pipe(ff: &Path, card: &Path, out: &Path, separate: bool, secs: f64) -> Duration {
    use crate::audio::{AudioSourceSpec, Capture, Opener};
    use crate::audiopipe::{AudioPipe, LogFn};
    struct Silent;
    impl Capture for Silent {
        fn name(&self) -> String {
            "沒有聲音".into()
        }
        fn read(&mut self, _visit: &mut crate::audio::Visit) -> Result<(), String> {
            Ok(())
        }
    }
    let opener: Opener = Arc::new(|_: &AudioSourceSpec| -> Result<Box<dyn Capture>, String> { Ok(Box::new(Silent)) });
    let log: LogFn = Arc::new(|_, _| {});
    let specs = vec![AudioSourceSpec { loopback: true, mic_id: String::new(), app: String::new() }, AudioSourceSpec { loopback: false, mic_id: String::new(), app: String::new() }];
    let pipe = Arc::new(AudioPipe::new(specs, opener, log, separate).unwrap());
    let config = RecordConfig { audio: AudioConfig { system: true, mic: true, mic_id: String::new(), app: String::new() }, separate_audio: separate, ..record_config() };
    let (w, h) = (crate::audio_card::SIZE.0 as i32, crate::audio_card::SIZE.1 as i32);
    let plan = CapturePlan { rect: Rect { x: 0, y: 0, width: w, height: h }, dda: None, monitors: vec![], out_width: w, out_height: h, card: Some(card.display().to_string()) };
    let args = segment_args(&plan, &config, CaptureMethod::Gdigrab, &ENCODERS[0], &out.display().to_string(), Some(&pipe.input_args()), None).unwrap();
    let start = std::time::Instant::now();
    let mut child = Command::new(ff).args(&args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
    // 和錄影器一樣讀 showinfo 的 pts 交給聲音管線
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<Duration>();
    let p = pipe.clone();
    let tail = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::<String>::new()));
    let tail2 = tail.clone();
    let reader = std::thread::spawn(move || {
        use std::io::BufRead;
        let re = regex::Regex::new(r"Parsed_showinfo.*\bpts_time:\s*(-?[\d.]+)").unwrap();
        let mut first = true;
        for line in std::io::BufReader::new(stderr).lines().map_while(|l| l.ok()) {
            if !line.contains("Parsed_showinfo") {
                let mut t = tail2.lock().unwrap();
                t.push_back(line.clone());
                if t.len() > 15 {
                    t.pop_front();
                }
            }
            if let Some(c) = re.captures(&line) {
                p.on_video_frame(c[1].parse().unwrap_or(0.0), crate::clock::now_100ns());
                if first {
                    first = false;
                    let _ = tx.send(start.elapsed());
                }
            }
        }
    });
    let first = rx.recv_timeout(Duration::from_secs(10));
    let mut stdin = child.stdin.take().unwrap();
    if first.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        pipe.close();
        panic!("10 秒內沒有任何畫面：FFmpeg 在等聲音，聲音管線在等畫面（分軌 = {separate}）");
    }
    std::thread::sleep(Duration::from_secs_f64(secs));
    pipe.flush_to_now();
    stdin.write_all(b"q").unwrap();
    stdin.flush().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            panic!("FFmpeg 收到 q 後沒有結束（分軌 = {separate}）：{:?}", tail.lock().unwrap());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    pipe.close();
    let _ = reader.join();
    drop(stdin);
    first.unwrap()
}

/// 錄影時麥克風降噪：系統聲音與麥克風分開送進來，麥克風降噪後混成一條（分軌時是三條）
#[tokio::test]
async fn mic_denoise_while_recording() {
    let Some(ff) = ffmpeg() else { return };
    if !has(&ff, "-encoders", "libx264") || !has(&ff, "-filters", "afftdn") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let card = write_card(dir.path());
    // 系統聲音 440 Hz；麥克風 880 Hz 加上雜音
    let sys = "0.3*sin(440*2*PI*t)";
    let mic = "0.3*sin(880*2*PI*t)+0.05*(random(0)-0.5)";
    let src = format!("aevalsrc={sys}+{mic}|{sys}+{mic}|{sys}|{sys}|{mic}|{mic}:s=48000:c=5.1");
    for separate in [false, true] {
        let config =
            RecordConfig { audio: crate::types::AudioConfig { system: true, mic: true, mic_id: String::new(), app: String::new() }, separate_audio: separate, mic_denoise: true, ..record_config() };
        assert!(config.pipe_separate());
        let out = dir.path().join(format!("dn_{}.mp4", separate as u8));
        record_segment_with(&ff, &card, &out, 2.0, false, &config, &src);
        let i = probe(&ff, &out).await;
        assert_eq!((i.has_audio, i.audio_tracks), (Some(true), if separate { 3 } else { 1 }), "分軌 = {separate}");
        assert!(max_volume(&ff, &out) > -20.0);
    }
}

#[tokio::test]
async fn recording_with_the_audio_pipe_starts_right_away() {
    let Some(ff) = ffmpeg() else { return };
    // FFmpeg 6 以前要等每個輸入都有資料才處理畫面，聲音管線（等第一張畫面才送）沒辦法用
    if !has(&ff, "-encoders", "libx264") || ffmpeg_major(&ff).is_none_or(|v| v < 7) {
        eprintln!("略過：需要 FFmpeg 7 以上");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let card = write_card(dir.path());
    for separate in [false, true] {
        let out = dir.path().join(format!("seg_{}.mp4", separate as u8));
        let first = record_with_audio_pipe(&ff, &card, &out, separate, 3.0);
        assert!(first < Duration::from_secs(5), "第一張畫面花了 {first:?}（分軌 = {separate}）");
        let i = probe(&ff, &out).await;
        assert!(i.duration_sec.unwrap() > 2.0, "{:?}", i.duration_sec);
        assert_eq!((i.has_audio, i.audio_tracks), (Some(true), if separate { 3 } else { 1 }), "分軌 = {separate}");
    }
}
