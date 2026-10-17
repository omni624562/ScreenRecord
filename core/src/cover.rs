//! 影片封面：從影片挑一格，加上標題，做成 YouTube 用的 1280×720 封面圖（PNG，存在影片旁邊）；
//! 也可以寫進 MP4 當封面（檔案總管看到的縮圖就是它）。

use crate::annotate;
use crate::{tr, Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tiny_skia::{Color, FilterQuality, GradientStop, LinearGradient, Paint, Pixmap, PixmapPaint, Point, Rect, SpreadMode, Transform};

/// 封面的大小（YouTube 建議的 16:9）
pub const W: u32 = 1280;
pub const H: u32 = 720;

/// 標題怎麼放
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CoverStyle {
    /// 下方一條半透明深色帶，白色大字靠左
    #[default]
    Band,
    /// 整張變暗，大字放中間
    Center,
    /// 只有畫面，不加字
    Plain,
}

impl CoverStyle {
    pub const ALL: [CoverStyle; 3] = [CoverStyle::Band, CoverStyle::Center, CoverStyle::Plain];

    pub fn label(self) -> &'static str {
        match self {
            CoverStyle::Band => tr!("下方橫條", "Bottom band"),
            CoverStyle::Center => tr!("置中大字", "Centered"),
            CoverStyle::Plain => tr!("只有畫面", "Picture only"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Cover {
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default)]
    pub style: CoverStyle,
}

/// 畫出封面：畫面等比放大填滿 1280×720（多的部分裁掉，置中），再依樣式加上標題
pub fn render(frame: &Pixmap, c: &Cover) -> Option<Pixmap> {
    let mut pm = Pixmap::new(W, H)?;
    pm.fill(Color::BLACK);
    let (fw, fh) = (frame.width() as f32, frame.height() as f32);
    if fw >= 1.0 && fh >= 1.0 {
        let s = (W as f32 / fw).max(H as f32 / fh);
        let (dx, dy) = ((W as f32 - fw * s) / 2.0, (H as f32 - fh * s) / 2.0);
        let paint = PixmapPaint { quality: FilterQuality::Bicubic, ..Default::default() };
        pm.draw_pixmap(0, 0, frame.as_ref(), &paint, Transform::from_row(s, 0.0, 0.0, s, dx, dy), None);
    }
    let (title, subtitle) = (c.title.trim(), c.subtitle.trim());
    if c.style == CoverStyle::Plain || (title.is_empty() && subtitle.is_empty()) {
        return Some(pm);
    }
    let (wf, hf) = (W as f64, H as f64);
    // 寬度放不下時縮小（最多兩行）
    let fit = |text: &str, size: f64, color: &str, max_w: f64| {
        let text = crate::subtitles::wrap_lines(text);
        let mut a = annotate::plain_text(&text, size, color);
        if a.w > max_w {
            a = annotate::plain_text(&text, (size * max_w / a.w).max(12.0), color);
        }
        a
    };
    match c.style {
        CoverStyle::Band => {
            let max_w = wf * 0.9;
            let mut lines = vec![];
            if !title.is_empty() {
                lines.push(fit(title, hf * 0.11, "#FFFFFF", max_w));
            }
            if !subtitle.is_empty() {
                lines.push(fit(subtitle, hf * 0.05, "#D6DAE3", max_w));
            }
            let gap = hf * 0.015;
            let pad = hf * 0.05;
            let text_h: f64 = lines.iter().map(|a| a.h).sum::<f64>() + gap * lines.len().saturating_sub(1) as f64;
            let band_h = text_h + pad * 2.0;
            let top = hf - band_h;
            // 由上往下變深的帶子，上緣自然過渡
            fade_rect(&mut pm, top - hf * 0.08, hf, [0, 0, 0], 0.0, 0.78);
            let mut y = top + pad;
            for mut a in lines {
                a.x = (wf * 0.05).round();
                a.y = y.round();
                y += a.h + gap;
                annotate::draw(&mut pm, &a, Transform::identity());
            }
        }
        CoverStyle::Center => {
            fill(&mut pm, 0.0, 0.0, wf, hf, [0, 0, 0], 0.5);
            let max_w = wf * 0.86;
            let mut lines = vec![];
            if !title.is_empty() {
                lines.push(fit(title, hf * 0.14, "#FFFFFF", max_w));
            }
            if !subtitle.is_empty() {
                lines.push(fit(subtitle, hf * 0.06, "#E2E6EE", max_w));
            }
            let gap = hf * 0.03;
            let total: f64 = lines.iter().map(|a| a.h).sum::<f64>() + gap * lines.len().saturating_sub(1) as f64;
            let mut y = (hf - total) / 2.0;
            for mut a in lines {
                a.x = ((wf - a.w) / 2.0).round();
                a.y = y.round();
                y += a.h + gap;
                annotate::draw(&mut pm, &a, Transform::identity());
            }
        }
        CoverStyle::Plain => {}
    }
    Some(pm)
}

fn fill(pm: &mut Pixmap, x: f64, y: f64, w: f64, h: f64, rgb: [u8; 3], alpha: f32) {
    let mut paint = Paint::default();
    paint.set_color(Color::from_rgba(rgb[0] as f32 / 255.0, rgb[1] as f32 / 255.0, rgb[2] as f32 / 255.0, alpha).unwrap_or(Color::BLACK));
    if let Some(r) = Rect::from_xywh(x as f32, y as f32, w as f32, h as f32) {
        pm.fill_rect(r, &paint, Transform::identity(), None);
    }
}

/// 從 y0 到 y1 由 a0 漸變到 a1 的半透明色塊
fn fade_rect(pm: &mut Pixmap, y0: f64, y1: f64, rgb: [u8; 3], a0: f32, a1: f32) {
    let c = |a: f32| Color::from_rgba(rgb[0] as f32 / 255.0, rgb[1] as f32 / 255.0, rgb[2] as f32 / 255.0, a).unwrap_or(Color::BLACK);
    let Some(shader) = LinearGradient::new(
        Point::from_xy(0.0, y0 as f32),
        Point::from_xy(0.0, (y0 + (y1 - y0) * 0.35) as f32),
        vec![GradientStop::new(0.0, c(a0)), GradientStop::new(1.0, c(a1))],
        SpreadMode::Pad,
        Transform::identity(),
    ) else {
        return;
    };
    let paint = Paint { shader, anti_alias: false, ..Default::default() };
    if let Some(r) = Rect::from_xywh(0.0, y0.max(0.0) as f32, pm.width() as f32, (y1 - y0.max(0.0)) as f32) {
        pm.fill_rect(r, &paint, Transform::identity(), None);
    }
}

/// 封面圖的檔名：影片旁邊的「<影片名>_封面.png」
pub fn cover_path(video: &Path) -> PathBuf {
    let stem = video.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    video.with_file_name(format!("{stem}{}.png", tr!("_封面", "_cover")))
}

/// 把封面寫進 MP4（attached_pic）：只複製串流不重新壓縮；原本的封面換掉（0:V 不含封面圖）
pub fn embed_args(video: &str, png: &str, out: &str) -> Vec<String> {
    let mut a: Vec<String> = ["-hide_banner", "-nostats", "-loglevel", "error", "-i", video, "-i", png].iter().map(|s| s.to_string()).collect();
    a.extend(
        [
            "-map",
            "0:V",
            "-map",
            "0:a?",
            "-map",
            "0:s?",
            "-map",
            "1",
            "-c",
            "copy",
            "-disposition:v:1",
            "attached_pic",
            "-map_metadata",
            "0",
            "-map_chapters",
            "0",
            "-movflags",
            "+faststart",
            "-y",
            out,
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    a
}

/// 讀出影片 t 秒的那一格（縮成寬 1280 以內，夠做封面也比較快）
pub async fn frame_at(ffmpeg: &Path, video: &str, t: f64) -> Result<Pixmap> {
    let out = std::env::temp_dir().join(format!("ScreenRecorder-cover-{}-{}.png", std::process::id(), crate::paths::now_ms()));
    let t = if t.is_finite() { t.max(0.0) } else { 0.0 };
    let args: Vec<String> = ["-hide_banner", "-loglevel", "error"]
        .iter()
        .map(|s| s.to_string())
        .chain(crate::args::input_at(video, &format!("{t:.3}")))
        .chain(["-frames:v", "1", "-vf", "scale='min(1280,iw)':-2", "-update", "1", "-y"].iter().map(|s| s.to_string()))
        .chain(std::iter::once(out.display().to_string()))
        .collect();
    let r = crate::process::run(ffmpeg, &args, Duration::from_secs(30)).await;
    if r.code != 0 {
        crate::info!("[封面] 讀取 {video} 的 {t:.2} 秒失敗：{}", r.stderr.trim());
    }
    let pm = Pixmap::load_png(&out);
    let _ = std::fs::remove_file(&out);
    pm.map_err(|_| Error::other(tr!("讀不到這一格畫面，請換個時間點再試", "Couldn't read this frame. Try a different moment")))
}

/// 存好的結果
#[derive(Debug, Clone, PartialEq)]
pub struct Saved {
    pub png: PathBuf,
    /// 有寫進影片
    pub embedded: bool,
    /// 沒寫進影片的原因
    pub embed_error: Option<String>,
}

/// 存成影片旁邊的 PNG；embed = 也寫進影片（先寫到暫存檔，成功才換掉原檔）
pub async fn save(ffmpeg: Option<&Path>, video: &str, pm: &Pixmap, embed: bool) -> Result<Saved> {
    let png = cover_path(Path::new(video));
    let data = pm.encode_png().map_err(|e| Error::other(e.to_string()))?;
    tokio::fs::write(&png, data).await.map_err(|e| Error::other(e.to_string()))?;
    crate::info!("[封面] 已存 {}", png.display());
    let mut saved = Saved { png: png.clone(), embedded: false, embed_error: None };
    if !embed {
        return Ok(saved);
    }
    match embed_png(ffmpeg, video, &png).await {
        Ok(()) => {}
        Err(e) => {
            saved.embed_error = Some(e);
            return Ok(saved);
        }
    }
    saved.embedded = true;
    Ok(saved)
}

/// 可以寫入封面（縮圖）的影片
pub fn can_embed(video: &str) -> bool {
    let lower = video.to_ascii_lowercase();
    lower.ends_with(".mp4") || lower.ends_with(".m4v") || lower.ends_with(".mov")
}

/// 把 PNG 寫進影片當封面（先寫到暫存檔，成功才換掉原檔，修改時間保留）；失敗時回傳給使用者看的原因
pub async fn embed_png(ffmpeg: Option<&Path>, video: &str, png: &Path) -> std::result::Result<(), String> {
    if !can_embed(video) {
        return Err(tr!("只有 MP4 影片可以寫入封面", "Only MP4 videos can have a cover written into them").into());
    }
    let Some(ffmpeg) = ffmpeg else {
        return Err(tr!("找不到 FFmpeg", "FFmpeg not found").into());
    };
    let v = Path::new(video);
    let tmp = v.with_file_name(format!(".{}.cover-tmp.mp4", v.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()));
    let args = embed_args(video, &png.display().to_string(), &tmp.display().to_string());
    let r = crate::process::run(ffmpeg, &args, Duration::from_secs(600)).await;
    let ok = r.code == 0 && tokio::fs::metadata(&tmp).await.map(|m| m.len() > 0).unwrap_or(false);
    if !ok {
        let _ = tokio::fs::remove_file(&tmp).await;
        crate::info!("[封面] 寫入影片失敗：{}", r.stderr.trim());
        return Err(tr!("無法寫進影片（詳見記錄檔）", "Couldn't write it into the video (see the log file)").into());
    }
    // 原檔的修改時間保留（清單依時間排序，不要跳到最上面）
    let mtime = std::fs::metadata(v).and_then(|m| m.modified()).ok();
    if let Err(e) = tokio::fs::rename(&tmp, v).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        crate::info!("[封面] 換掉原檔失敗：{e}");
        return Err(tr!("影片正在使用中（播放中或被其他程式開啟），沒有寫進影片", "The video is in use (playing or open in another program), so the cover wasn't written into it").into());
    }
    if let (Some(t), Ok(f)) = (mtime, std::fs::File::options().write(true).open(v)) {
        let _ = f.set_modified(t);
    }
    crate::info!("[封面] 已寫進 {video}");
    Ok(())
}

/// 設為封面：影片 t 秒的那一格（不加字、不另存圖檔）寫進影片當縮圖。影片內容不變，
/// 檔案總管等會讀封面的地方顯示這一格
pub async fn set_thumbnail(ffmpeg: Option<&Path>, video: &str, t: f64) -> Result<()> {
    let ff = ffmpeg.ok_or_else(|| Error::config(tr!("找不到 FFmpeg", "FFmpeg not found")))?;
    if !can_embed(video) {
        return Err(Error::config(tr!("只有 MP4 影片可以設定封面", "Only MP4 videos can have a cover")));
    }
    // 播到最後停下來時時間剛好是影片長度，那裡已經沒有畫面：往前退一點再讀
    let pm = match frame_at(ff, video, t).await {
        Ok(pm) => pm,
        Err(_) if t > 0.0 => frame_at(ff, video, (t - 0.5).max(0.0)).await?,
        Err(e) => return Err(e),
    };
    let png = std::env::temp_dir().join(format!("ScreenRecorder-thumb-{}-{}.png", std::process::id(), crate::paths::now_ms()));
    let data = pm.encode_png().map_err(|e| Error::other(e.to_string()))?;
    tokio::fs::write(&png, data).await.map_err(|e| Error::other(e.to_string()))?;
    let r = embed_png(ffmpeg, video, &png).await;
    let _ = tokio::fs::remove_file(&png).await;
    r.map_err(Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(pm: &Pixmap, x: u32, y: u32) -> [u8; 3] {
        let p = pm.pixel(x, y).unwrap();
        [p.red(), p.green(), p.blue()]
    }

    fn frame(w: u32, h: u32) -> Pixmap {
        // 上半紅、下半綠
        let mut pm = Pixmap::new(w, h).unwrap();
        pm.fill(Color::from_rgba8(220, 30, 30, 255));
        let mut g = Paint::default();
        g.set_color_rgba8(30, 200, 30, 255);
        pm.fill_rect(Rect::from_xywh(0.0, h as f32 / 2.0, w as f32, h as f32 / 2.0).unwrap(), &g, Transform::identity(), None);
        pm
    }

    #[test]
    fn frame_fills_the_cover_and_styles_draw_text() {
        // 4:3 的畫面：放大填滿 16:9，上下各裁掉一點，不留黑邊
        let f = frame(800, 600);
        let plain = render(&f, &Cover { title: "會被忽略".into(), style: CoverStyle::Plain, ..Default::default() }).unwrap();
        assert_eq!((plain.width(), plain.height()), (W, H));
        assert_eq!(px(&plain, 5, 5), [220, 30, 30]);
        assert_eq!(px(&plain, 1275, 715), [30, 200, 30]);
        // 下方橫條：底部變暗、有白字
        let band = render(&f, &Cover { title: "報價單怎麼填".into(), subtitle: "業務部教學".into(), style: CoverStyle::Band }).unwrap();
        assert!(px(&band, 1270, 712)[1] < 80, "{:?}", px(&band, 1270, 712));
        assert_eq!(px(&band, 640, 100), [220, 30, 30], "上面不受影響");
        let white = |pm: &Pixmap, ys: std::ops::Range<u32>| (0..W).step_by(2).flat_map(|x| ys.clone().step_by(2).map(move |y| (x, y))).filter(|&(x, y)| px(pm, x, y).iter().all(|v| *v > 230)).count();
        assert!(white(&band, 480..720) > 300);
        // 置中大字：整張變暗，中間有白字
        let center = render(&f, &Cover { title: "報價單怎麼填".into(), subtitle: String::new(), style: CoverStyle::Center }).unwrap();
        assert!(px(&center, 5, 5)[0] < 130);
        assert!(white(&center, 260..460) > 300);
        // 沒有字：和只有畫面一樣
        let empty = render(&f, &Cover { style: CoverStyle::Band, ..Default::default() }).unwrap();
        assert_eq!(empty.data(), plain.data());
    }

    #[test]
    fn long_titles_fit_and_paths_and_args() {
        let f = frame(1920, 1080);
        let c = Cover { title: "這是一個非常非常非常非常非常非常非常非常非常非常非常非常長的標題".into(), subtitle: String::new(), style: CoverStyle::Band };
        let pm = render(&f, &c).unwrap();
        // 右邊緣留白：字沒有超出畫面
        assert!((700..720).all(|y| px(&pm, 1279, y).iter().all(|v| *v < 230)));
        assert_eq!(cover_path(Path::new("/v/Rec_2026-10-15.mp4")), Path::new("/v/Rec_2026-10-15_封面.png"));
        let a = embed_args("in.mp4", "c.png", "out.mp4");
        let s = a.join(" ");
        assert!(s.contains("-map 0:V -map 0:a? -map 0:s? -map 1 -c copy -disposition:v:1 attached_pic"), "{s}");
        assert!(s.ends_with("-y out.mp4"));
    }
}
