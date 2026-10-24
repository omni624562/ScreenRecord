//! 步驟截圖：開始後每按一下滑鼠就截一張（標出點的位置），完成後先在操作視窗整理（刪掉、調整順序），
//! 再做成一份教學文件（HTML）：每個步驟一段說明與一張圖，圖片都包在同一個檔案裡（單獨傳給別人也看得到）。
//! 說明可以直接在瀏覽器裡修改，再列印成 PDF 或複製到 Word。

use crate::{tr, trf};
use std::path::{Path, PathBuf};

/// 一個步驟
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// 點的時間（時:分:秒）
    pub time: String,
    /// 點到的視窗標題
    pub window: String,
    /// 圖檔（相對於儲存資料夾的路徑：資料夾/step_NN.png）
    pub image: String,
    /// 說明（從錄影的操作步驟做的才有，例如「點「檔案」（選單）」）；空的 = 用視窗標題組成
    pub text: String,
}

impl Step {
    /// 這一步的說明
    pub fn caption(&self) -> String {
        if self.text.trim().is_empty() {
            caption(&self.window)
        } else {
            self.text.trim().to_string()
        }
    }
}

/// 截好、還沒做成文件的步驟（在操作視窗整理）
#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    /// 儲存資料夾（文件放這裡）
    pub dir: PathBuf,
    /// 文件名稱（教學_日期_時間）；截圖暫時放在同名的資料夾
    pub name: String,
    /// 開始的時間（文件標題與日期用）
    pub date: String,
    pub steps: Vec<Step>,
}

impl Draft {
    /// 放截圖的資料夾
    pub fn folder(&self) -> PathBuf {
        self.dir.join(&self.name)
    }

    pub fn image_path(&self, s: &Step) -> PathBuf {
        self.dir.join(&s.image)
    }

    /// 預設的文件標題
    pub fn title(&self) -> String {
        trf!("操作步驟（{}）", "Steps ({})", self.date)
    }

    /// 刪掉截圖（取消、或已經做成文件）
    pub fn discard(&self) {
        if !self.name.is_empty() {
            let _ = std::fs::remove_dir_all(self.folder());
        }
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// 步驟的說明：「在「視窗」按一下」
pub fn caption(window: &str) -> String {
    let w = window.trim();
    if w.is_empty() {
        tr!("按一下", "Click").into()
    } else {
        trf!("在「{w}」按一下", "Click in “{w}”")
    }
}

/// 教學文件（HTML）；srcs = 每一步的圖（data: 網址或路徑）
pub fn html(title: &str, date: &str, steps: &[Step], srcs: &[String]) -> String {
    let mut items = String::new();
    for (i, (s, src)) in steps.iter().zip(srcs).enumerate() {
        items.push_str(&trf!(
            "<li><p class=\"cap\" contenteditable=\"true\"><b>步驟 {}</b>　{}</p><p class=\"time\">{}</p><img src=\"{}\" alt=\"步驟 {}\"></li>\n",
            "<li><p class=\"cap\" contenteditable=\"true\"><b>Step {}</b> {}</p><p class=\"time\">{}</p><img src=\"{}\" alt=\"Step {}\"></li>\n",
            i + 1,
            esc(&s.caption()),
            esc(&s.time),
            esc(src),
            i + 1
        ));
    }
    let (d, n) = (esc(date), steps.len());
    let meta = if crate::i18n::is_en() { format!("{d} · {n} step{}", if n == 1 { "" } else { "s" }) } else { format!("{d}・共 {n} 個步驟") };
    let tip = tr!(
        "文字可以直接按一下修改（例如補上要輸入什麼）。在瀏覽器裡改的文字不會自動存檔：改好後用瀏覽器的「列印」存成 PDF，或全選複製貼到 Word。圖片都包在這個檔案裡，單獨傳給別人也看得到。",
        "Click any text to edit it (e.g. add what to type). Changes made in the browser aren't saved automatically: when you're done, use the browser's “Print” to save it as a PDF, or select all and paste it into Word. The pictures are inside this file, so you can send it on its own."
    );
    format!(
        r#"<!doctype html>
<html lang="{lang}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{t}</title>
<style>
body {{ font-family: "Microsoft JhengHei UI", "Noto Sans TC", sans-serif; max-width: 1100px; margin: 32px auto; padding: 0 20px; color: #1f2328; background: #fff; }}
h1 {{ font-size: 26px; margin: 0 0 6px; }}
.meta {{ color: #6b7280; margin: 0 0 24px; }}
.tip {{ background: #eef6ff; border: 1px solid #c7defa; border-radius: 8px; padding: 10px 14px; color: #1d4f91; }}
ol {{ list-style: none; padding: 0; }}
li {{ margin: 0 0 36px; page-break-inside: avoid; }}
.cap {{ font-size: 18px; margin: 0 0 4px; outline: none; }}
.cap:focus {{ background: #fff8d6; }}
.time {{ color: #9aa1ab; font-size: 13px; margin: 0 0 8px; }}
img {{ max-width: 100%; border: 1px solid #d0d4da; border-radius: 6px; }}
@media print {{ .tip {{ display: none; }} body {{ margin: 0; }} }}
</style>
</head>
<body>
<h1 contenteditable="true">{t}</h1>
<p class="meta">{meta}</p>
<p class="tip">{tip}</p>
<ol>
{items}</ol>
</body>
</html>
"#,
        lang = tr!("zh-Hant-TW", "en"),
        t = esc(title),
    )
}

/// 截圖時浮在畫面下方的小工具列：按鈕編號
pub const BAR_FINISH: u32 = 1;
pub const BAR_CANCEL: u32 = 2;
pub const BAR_DISCARD: u32 = 3;
pub const BAR_KEEP: u32 = 4;

/// 小工具列的內容：「步驟截圖中・已截 N 步 [完成] [取消]」；按了取消（已經有步驟）時先問要不要放棄
pub fn bar(count: u32, confirm: bool) -> crate::bubble::Content {
    let en = crate::i18n::is_en();
    if confirm {
        let q = if en { format!("Discard the {count} captured step{}?", if count == 1 { "" } else { "s" }) } else { format!("要放棄已截的 {count} 步嗎？") };
        return crate::bubble::Content {
            big: None,
            lines: vec![q, tr!("截好的圖會刪掉，不會做成文件", "The screenshots will be deleted").into()],
            buttons: vec![(BAR_KEEP, tr!("繼續截圖", "Keep capturing").into(), true), (BAR_DISCARD, tr!("放棄", "Discard").into(), false)],
        };
    }
    let head = if en { format!("Capturing steps · {count} captured") } else { format!("步驟截圖中・已截 {count} 步") };
    crate::bubble::Content {
        big: None,
        lines: vec![head, tr!("每按一下滑鼠就截一張", "Each mouse click takes a screenshot").into()],
        buttons: vec![(BAR_FINISH, tr!("完成", "Finish").into(), true), (BAR_CANCEL, tr!("取消", "Cancel").into(), false)],
    }
}

/// 圖片太寬（4K 螢幕）時縮到這個寬度再放進文件：檔案小一點，瀏覽器裡看起來一樣
const DOC_MAX_W: u32 = 1920;

pub(crate) fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

/// 一步的圖放進文件：data: 網址（PNG）
fn data_uri(path: &Path) -> Result<String, String> {
    Ok(format!("data:image/png;base64,{}", base64(&doc_png(path)?)))
}

/// 放進文件的 PNG：太寬時縮小
fn doc_png(path: &Path) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| crate::error::plain(&e))?;
    let wide = image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format().ok().and_then(|r| r.into_dimensions().ok()).is_some_and(|(w, _)| w > DOC_MAX_W);
    Ok(if wide {
        let img = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
        let img = img.resize(DOC_MAX_W, u32::MAX, image::imageops::FilterType::Triangle);
        let mut out = std::io::Cursor::new(vec![]);
        img.write_to(&mut out, image::ImageFormat::Png).map_err(|e| e.to_string())?;
        out.into_inner()
    } else {
        bytes
    })
}

/// 教學文件的格式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DocFormat {
    /// 一個 HTML 檔（圖片包在裡面，用瀏覽器開啟、可以直接改文字）
    #[default]
    Html,
    /// Word（.docx）
    Word,
    /// PowerPoint（.pptx）：一步一張投影片
    PowerPoint,
    /// 影片（.mp4）：一步一張畫面加上說明，可以唸出來（App::steps_build_video）
    Video,
}

impl DocFormat {
    fn ext(self) -> &'static str {
        match self {
            DocFormat::Html => "html",
            DocFormat::Word => "docx",
            DocFormat::PowerPoint => "pptx",
            DocFormat::Video => "mp4",
        }
    }
}

/// 做成 Word 或 PowerPoint 的內容
fn office(draft: &Draft, title: &str, steps: &[Step], format: DocFormat) -> Result<Vec<u8>, String> {
    let mut items = Vec::with_capacity(steps.len());
    for s in steps {
        let path = draft.image_path(s);
        let png = std::fs::read(&path).map_err(|e| trf!("無法讀取「{}」：{e}", "Couldn't read “{}”: {e}", s.image))?;
        let (width, height) = image::ImageReader::new(std::io::Cursor::new(&png)).with_guessed_format().ok().and_then(|r| r.into_dimensions().ok()).unwrap_or((1280, 720));
        items.push(crate::ooxml::Item { caption: s.caption(), png, width, height });
    }
    Ok(match format {
        DocFormat::PowerPoint => crate::ooxml::pptx(title, &draft.date, &items),
        _ => crate::ooxml::docx(title, &draft.date, &items),
    })
}

/// 做成教學文件：steps 是整理後（刪掉、調整順序）的步驟；寫成 儲存資料夾/文件名稱.html（已經有同名檔案時加上編號），
/// 成功後刪掉放截圖的資料夾。回傳文件的路徑。
pub fn build(draft: &Draft, title: &str, steps: &[Step]) -> Result<PathBuf, String> {
    build_as(draft, title, steps, DocFormat::Html)
}

/// 同上，指定格式（HTML、Word、PowerPoint）
pub fn build_as(draft: &Draft, title: &str, steps: &[Step], format: DocFormat) -> Result<PathBuf, String> {
    let title = if title.trim().is_empty() { draft.title() } else { title.trim().to_string() };
    let doc: Vec<u8> = match format {
        DocFormat::Html => {
            let srcs = steps.iter().map(|s| data_uri(&draft.image_path(s)).map_err(|e| trf!("無法讀取「{}」：{e}", "Couldn't read “{}”: {e}", s.image))).collect::<Result<Vec<_>, _>>()?;
            html(&title, &draft.date, steps, &srcs).into_bytes()
        }
        DocFormat::Video => return Err(tr!("影片要用 App::steps_build_video 做", "Videos are made with steps_build_video").into()),
        _ => office(draft, &title, steps, format)?,
    };
    let ext = format.ext();
    let mut path = draft.dir.join(format!("{}.{ext}", draft.name));
    let mut n = 2;
    while path.exists() {
        path = draft.dir.join(format!("{} ({n}).{ext}", draft.name));
        n += 1;
    }
    let tmp = path.with_extension(format!("{ext}.tmp"));
    std::fs::write(&tmp, doc).and_then(|_| std::fs::rename(&tmp, &path)).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        trf!("無法儲存教學文件：{e}", "Couldn't save the step-by-step guide: {e}")
    })?;
    draft.discard();
    Ok(path)
}

/// 在 PNG 上圈出點的位置（x、y 是在畫面裡的位置 0～1），直接覆寫
pub fn ring_png(path: &Path, x: f64, y: f64) -> Result<(), String> {
    use tiny_skia::{Color, Paint, PathBuilder, Pixmap, Stroke, Transform};
    let mut pm = Pixmap::load_png(path).map_err(|e| e.to_string())?;
    let (w, h) = (pm.width() as f32, pm.height() as f32);
    let r = (w.min(h) * 0.035).max(12.0);
    let (cx, cy) = (x.clamp(0.0, 1.0) as f32 * w, y.clamp(0.0, 1.0) as f32 * h);
    let circle = PathBuilder::from_circle(cx, cy, r).ok_or("circle")?;
    let mut paint = Paint { anti_alias: true, ..Default::default() };
    // 白邊＋紅圈：深色、淺色畫面都看得清楚
    paint.set_color(Color::from_rgba8(255, 255, 255, 230));
    pm.stroke_path(&circle, &paint, &Stroke { width: r * 0.32, ..Default::default() }, Transform::identity(), None);
    paint.set_color(Color::from_rgba8(230, 40, 40, 255));
    pm.stroke_path(&circle, &paint, &Stroke { width: r * 0.18, ..Default::default() }, Transform::identity(), None);
    pm.save_png(path).map_err(|e| e.to_string())
}

// ───────────── 做成影片 ─────────────

/// 影片每一步至少顯示幾秒
pub const VIDEO_SECS: [f64; 4] = [2.0, 3.0, 5.0, 8.0];
/// 影片的大小與每秒張數
pub const VIDEO_SIZE: (u32, u32) = (1920, 1080);
const VIDEO_FPS: f64 = 30.0;
/// 淡入淡出幾秒
const VIDEO_FADE: f64 = 0.4;

/// 影片的一格畫面：深色背景，截圖放在上方（縮放到放得下），下方是「步驟 N：說明」
pub fn video_frame(image: &Path, n: usize, text: &str, w: u32, h: u32) -> Result<tiny_skia::Pixmap, String> {
    use tiny_skia::{Color, FilterQuality, Pixmap, PixmapPaint, Transform};
    let mut pm = Pixmap::new(w, h).ok_or("frame")?;
    pm.fill(Color::from_rgba8(24, 26, 31, 255));
    let shot = Pixmap::load_png(image).map_err(|e| trf!("無法讀取截圖：{e}", "Couldn't read the screenshot: {e}"))?;
    let (wf, hf) = (w as f32, h as f32);
    let bar = hf * 0.16;
    let (aw, ah) = (wf * 0.92, hf - bar - hf * 0.06);
    let k = (aw / shot.width() as f32).min(ah / shot.height() as f32).min(2.0);
    let (sw, sh) = (shot.width() as f32 * k, shot.height() as f32 * k);
    let (x, y) = ((wf - sw) / 2.0, hf * 0.04 + (ah - sh) / 2.0);
    let paint = PixmapPaint { quality: FilterQuality::Bicubic, ..Default::default() };
    pm.draw_pixmap(0, 0, shot.as_ref(), &paint, Transform::from_row(k, 0.0, 0.0, k, x, y), None);
    // 說明（太長時縮小）
    let label = trf!("步驟 {n}：{}", "Step {n}: {}", text.trim());
    let size = (hf * 0.05) as f64;
    let mut a = crate::annotate::plain_text(&crate::subtitles::wrap_lines(&label), size, "#ffffff");
    let max_w = wf as f64 * 0.9;
    if a.w > max_w {
        a = crate::annotate::plain_text(&crate::subtitles::wrap_lines(&label), (size * max_w / a.w).max(10.0), "#ffffff");
    }
    a.x = ((wf as f64 - a.w) / 2.0).round();
    a.y = ((hf - bar) as f64 + (bar as f64 - a.h) / 2.0).round();
    crate::annotate::draw(&mut pm, &a, Transform::identity());
    Ok(pm)
}

/// 做影片的材料：(畫面 PNG, 秒數) 與每一步的語音 WAV
pub type VideoParts = (Vec<(String, f64)>, Vec<Option<String>>);

/// 一步要顯示幾秒：至少 min_secs，有語音時唸完再停一下
pub fn step_secs(min_secs: f64, voice_ms: Option<u64>) -> f64 {
    voice_ms.map_or(min_secs, |ms| min_secs.max(ms as f64 / 1000.0 + 0.8))
}

/// 接成影片的 FFmpeg 參數：frames = (畫面 PNG, 秒數)，voices = 每一步的語音 WAV（沒有時 None）；
/// 每一步淡入淡出，有任何語音時加上聲音（沒有語音的步驟是靜音）
pub fn video_args(frames: &[(String, f64)], voices: &[Option<String>], out: &str) -> Vec<String> {
    use crate::format::num;
    let mut a: Vec<String> = ["-hide_banner", "-nostats", "-loglevel", "error"].iter().map(|s| s.to_string()).collect();
    for (png, secs) in frames {
        a.extend(["-loop".into(), "1".into(), "-framerate".into(), num(VIDEO_FPS), "-t".into(), num(*secs), "-i".into(), png.clone()]);
    }
    let audio = voices.iter().any(Option::is_some);
    let mut vi = frames.len();
    let mut g = vec![];
    let mut pairs = String::new();
    for (i, (_, secs)) in frames.iter().enumerate() {
        let d = VIDEO_FADE.min(secs / 4.0);
        g.push(format!("[{i}:v]format=yuv420p,fade=t=in:st=0:d={},fade=t=out:st={}:d={}[v{i}]", num(d), num(secs - d), num(d)));
        pairs.push_str(&format!("[v{i}]"));
        if audio {
            match voices.get(i).cloned().flatten() {
                Some(w) => {
                    a.extend(["-i".into(), w]);
                    g.push(format!("[{vi}:a]aformat=sample_rates=48000:channel_layouts=stereo,adelay=300|300,apad,atrim=0:{}[a{i}]", num(*secs)));
                    vi += 1;
                }
                None => g.push(format!("anullsrc=r=48000:cl=stereo,atrim=0:{}[a{i}]", num(*secs))),
            }
            pairs.push_str(&format!("[a{i}]"));
        }
    }
    g.push(format!("{pairs}concat=n={}:v=1:a={}[vout]{}", frames.len(), u8::from(audio), if audio { "[aout]" } else { "" }));
    a.extend(["-filter_complex".into(), g.join(";"), "-map".into(), "[vout]".into()]);
    if audio {
        a.extend(["-map".into(), "[aout]".into(), "-c:a".into(), "aac".into(), "-b:a".into(), "160k".into()]);
    }
    a.extend(["-c:v", "libx264", "-preset", "veryfast", "-crf", "20", "-pix_fmt", "yuv420p", "-r", "30", "-movflags", "+faststart", "-y"].iter().map(|s| s.to_string()));
    a.push(out.into());
    a
}

/// 整理步驟時的小圖（未預乘 RGBA）：長邊縮到 max 以內
pub fn thumb(path: &Path, max: u32) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::open(path).ok()?.thumbnail(max, max).to_rgba8();
    let (w, h) = img.dimensions();
    Some((w, h, img.into_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caption_prefers_recorded_text() {
        let mut s = Step { time: "0:05".into(), window: "記事本".into(), image: "a/step_01.png".into(), text: String::new() };
        assert_eq!(s.caption(), caption("記事本"));
        s.text = " 點「存檔」（按鈕） ".into();
        assert_eq!(s.caption(), "點「存檔」（按鈕）");
    }

    #[test]
    fn ring_marks_the_click() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("f.png");
        let mut pm = tiny_skia::Pixmap::new(400, 200).unwrap();
        pm.fill(tiny_skia::Color::BLACK);
        pm.save_png(&png).unwrap();
        ring_png(&png, 0.5, 0.5).unwrap();
        let pm = tiny_skia::Pixmap::load_png(&png).unwrap();
        // 圈上（半徑約 12 像素）有紅色，圓心沒有
        let at = |x: u32, y: u32| pm.pixel(x, y).unwrap();
        assert!(at(200 + 12, 100).red() > 150);
        assert_eq!(at(200, 100).red(), 0);
    }

    #[test]
    fn makes_a_video_in_ffmpeg() {
        let dir = tempfile::tempdir().unwrap();
        let shot = dir.path().join("s.png");
        let mut pm = tiny_skia::Pixmap::new(800, 500).unwrap();
        pm.fill(tiny_skia::Color::from_rgba8(240, 240, 240, 255));
        pm.save_png(&shot).unwrap();
        let f = video_frame(&shot, 1, "在「記事本」按一下", 640, 360).unwrap();
        // 上方是截圖（淺色）、下方說明列是深色背景
        assert!(f.pixel(320, 150).unwrap().red() > 200 && f.pixel(5, 355).unwrap().red() < 60);
        assert_eq!(step_secs(3.0, None), 3.0);
        assert_eq!(step_secs(3.0, Some(4000)), 4.8);
        let Some(ffmpeg) = std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("ffmpeg")).find(|p| p.is_file())) else { return };
        let (a, b) = (dir.path().join("f1.png"), dir.path().join("f2.png"));
        f.save_png(&a).unwrap();
        f.save_png(&b).unwrap();
        let wav = dir.path().join("v.wav");
        std::fs::write(&wav, crate::narration::wav_mono16(&[0.2; 24000], 48000)).unwrap();
        let out = dir.path().join("out.mp4");
        let frames = vec![(a.display().to_string(), 2.0), (b.display().to_string(), 1.5)];
        for (voices, audio) in [(vec![Some(wav.display().to_string()), None], true), (vec![None, None], false)] {
            let args = video_args(&frames, &voices, out.to_str().unwrap());
            let r = std::process::Command::new(&ffmpeg).args(&args).output().unwrap();
            assert!(r.status.success(), "{}", String::from_utf8_lossy(&r.stderr));
            let probe = std::process::Command::new(&ffmpeg).args(["-hide_banner", "-i", out.to_str().unwrap()]).output().unwrap();
            let info = crate::args::parse_media_info(&String::from_utf8_lossy(&probe.stderr));
            assert!((info.duration_sec.unwrap() - 3.5).abs() < 0.15, "{info:?}");
            assert_eq!(info.has_audio, audio);
        }
    }

    #[test]
    fn builds_the_document() {
        let steps = vec![
            Step { time: "14:30:05".into(), window: "設定 <進階>".into(), image: "教學_1/step_01.png".into(), text: String::new() },
            Step { time: "14:30:09".into(), window: " ".into(), image: "教學_1/step_02.png".into(), text: String::new() },
        ];
        let h = html("登入流程", "2026/10/10", &steps, &["data:image/png;base64,AAAA".into(), "b.png".into()]);
        assert!(h.contains("<title>登入流程</title>"));
        assert!(h.contains("共 2 個步驟"));
        assert!(h.contains("<b>步驟 1</b>　在「設定 &lt;進階&gt;」按一下"));
        assert!(h.contains("<b>步驟 2</b>　按一下"));
        assert!(h.contains("src=\"data:image/png;base64,AAAA\""));
        assert!(h.contains("contenteditable=\"true\""));
        assert!(h.contains("在瀏覽器裡改的文字不會自動存檔"));
    }

    #[test]
    fn bar_asks_before_discarding() {
        let b = bar(3, false);
        assert_eq!(b.lines[0], "步驟截圖中・已截 3 步");
        assert_eq!(b.buttons.iter().map(|b| b.0).collect::<Vec<_>>(), [BAR_FINISH, BAR_CANCEL]);
        let b = bar(3, true);
        assert_eq!(b.lines[0], "要放棄已截的 3 步嗎？");
        assert_eq!(b.buttons.iter().map(|b| b.0).collect::<Vec<_>>(), [BAR_KEEP, BAR_DISCARD]);
        assert!(crate::bubble::render(&b, 1.0, None).is_some());
    }

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xff, 0xfe, 0x00]), "//4A");
    }

    #[test]
    fn builds_one_file_and_removes_the_pictures() {
        let dir = std::env::temp_dir().join(format!("steps-test-{}", std::process::id()));
        let draft = Draft { dir: dir.clone(), name: "教學_測試".into(), date: "2026/10/13 09:00".into(), steps: vec![] };
        std::fs::create_dir_all(draft.folder()).unwrap();
        let mut steps = vec![];
        for (i, w) in [(1, 40), (2, 2400)] {
            let img = image::RgbaImage::from_pixel(w, 30, image::Rgba([200, 30, 30, 255]));
            img.save(draft.folder().join(format!("step_0{i}.png"))).unwrap();
            steps.push(Step { time: format!("09:00:0{i}"), window: "記事本".into(), image: format!("教學_測試/step_0{i}.png"), text: String::new() });
        }
        let t = thumb(&draft.image_path(&steps[1]), 240).unwrap();
        assert_eq!((t.0, t.1, t.2.len()), (240, 3, 240 * 3 * 4));
        // 太寬的圖縮到 1920 再放進文件
        let png = doc_png(&draft.image_path(&steps[1])).unwrap();
        assert_eq!(image::load_from_memory(&png).unwrap().width(), 1920);
        assert_eq!(doc_png(&draft.image_path(&steps[0])).unwrap(), std::fs::read(draft.image_path(&steps[0])).unwrap());
        // 只留第 2 步（順序、刪除由介面決定）
        let path = build(&draft, " ", &steps[1..]).unwrap();
        assert_eq!(path, dir.join("教學_測試.html"));
        let h = std::fs::read_to_string(&path).unwrap();
        assert!(h.contains("<title>操作步驟（2026/10/13 09:00）</title>") && h.contains("共 1 個步驟"));
        assert_eq!(h.matches("src=\"data:image/png;base64,").count(), 1);
        assert!(!draft.folder().exists());
        // 同名的文件已經有了：加上編號
        std::fs::create_dir_all(draft.folder()).unwrap();
        image::RgbaImage::from_pixel(4, 4, image::Rgba([0, 0, 0, 255])).save(draft.folder().join("step_01.png")).unwrap();
        assert_eq!(build(&draft, "標題", &steps[..1]).unwrap(), dir.join("教學_測試 (2).html"));
        // 圖不見了：說明哪一張
        let err = build(&draft, "標題", &steps[..1]).unwrap_err();
        assert!(err.contains("step_01.png"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
