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
    }
}

/// 用錄影的參數錄一個分段 secs 秒：crash = false 時像按下停止一樣送 q 收尾；true 時直接砍掉（當機、停電）
fn record_segment(ff: &Path, card: &Path, out: &Path, secs: f64, crash: bool) {
    let (w, h) = (crate::audio_card::SIZE.0 as i32, crate::audio_card::SIZE.1 as i32);
    let plan = CapturePlan { rect: Rect { x: 0, y: 0, width: w, height: h }, dda: None, monitors: vec![], out_width: w, out_height: h, card: Some(card.display().to_string()) };
    // 錄影時聲音以實際速度從 TCP 進來；這裡用 -re 讓測試音也照實際速度
    let audio: Vec<String> = ["-re", "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000,aformat=channel_layouts=stereo"].iter().map(|s| s.to_string()).collect();
    let args = segment_args(&plan, &record_config(), CaptureMethod::Gdigrab, &ENCODERS[0], &out.display().to_string(), Some(&audio), None).unwrap();
    let mut child = Command::new(ff).args(&args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
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

fn write_card(dir: &Path) -> PathBuf {
    let card = dir.join("card_src.png");
    crate::audio_card::render("2026-10-11 10:00").unwrap().save_png(&card).unwrap();
    card
}

async fn probe(ff: &Path, path: &Path) -> MediaInfo {
    MediaCache::default().probe(ff, &path.display().to_string()).await.unwrap()
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
    let mut expect = 0.0;
    for s in &segs {
        let i = probe(&ff, s).await;
        assert_eq!(i.has_audio, Some(true));
        assert_eq!((i.width, i.height), (Some(640), Some(360)));
        expect += i.duration_sec.unwrap();
    }
    assert!(expect > 3.5, "分段太短：{expect}");

    let out = dir.path().join("Rec_2026-10-11_10-00-00.mp4");
    let chapters = crate::chapters::from_markers(&[1.0, 3.0], expect);
    let files: Vec<String> = segs.iter().map(|s| s.display().to_string()).collect();
    let duration = crate::recorder::merge_segments(&ff, &files, &parts, &out, true, Some(crate::chapters::ffmetadata(&chapters, expect))).await.unwrap().unwrap();
    assert!(near(duration, expect, 0.3), "合併後 {duration} 秒，分段加起來 {expect} 秒");

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
        audio: AudioFx { denoise: true, normalize: true, mute: false },
        // 3～5 秒 4 倍速：2 秒變 0.5 秒
        fast: vec![FastRange { start_ms: 3000, end_ms: 5000, speed: 4 }],
        zoom: Some(ClickZoom { factor: 1.5, clicks: vec![[2.0, 0.3, 0.4]] }),
        frame: Some(crate::video_frame::VideoFrame::default()),
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
