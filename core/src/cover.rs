//! 影片封面：把影片的某一格寫進 MP4 當封面（檔案總管看到的縮圖就是它）；
//! 加速版、壓縮、剪輯另存時帶上原影片的封面。

use crate::{tr, Error, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tiny_skia::Pixmap;

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

/// 讀出影片 t 秒的那一格（縮成寬 1280 以內，夠當封面也比較快）
pub async fn frame_at(ffmpeg: &Path, video: &str, t: f64) -> Result<Pixmap> {
    let out = crate::paths::unique_temp("ScreenRecorder-cover", ".png");
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

/// 可以寫入封面（縮圖）的影片
pub fn can_embed(video: &str) -> bool {
    let lower = video.to_ascii_lowercase();
    lower.ends_with(".mp4") || lower.ends_with(".m4v") || lower.ends_with(".mov")
}

/// 把圖檔（PNG、JPEG）寫進影片當封面（先寫到暫存檔，成功才換掉原檔，修改時間保留）；失敗時回傳給使用者看的原因
pub async fn embed_png(ffmpeg: Option<&Path>, video: &str, png: &Path) -> std::result::Result<(), String> {
    if !can_embed(video) {
        return Err(tr!("只有 MP4 影片可以寫入封面", "Only MP4 videos can have a cover written into them").into());
    }
    let Some(ffmpeg) = ffmpeg else {
        return Err(tr!("找不到 FFmpeg", "FFmpeg not found").into());
    };
    let png = png.display().to_string();
    rewrite_in_place(ffmpeg, video, |tmp| embed_args(video, &png, tmp)).await?;
    crate::info!("[封面] 已寫進 {video}");
    Ok(())
}

/// 取出 from 的封面（有設定過才有）存成 JPEG，給加速版、壓縮另存時帶上封面用。
/// width：縮到這個寬度以內（和輸出一樣寬；0 = 不縮）。回傳暫存檔與它的大小；沒有封面時 None
pub async fn extract_cover(ffmpeg: &Path, from: &str, to: &Path, width: i32) -> Option<(PathBuf, u64)> {
    if !can_embed(from) || !can_embed(&to.display().to_string()) {
        return None;
    }
    // 暫存檔放在輸出旁邊（隱藏檔名，用完就刪）
    let jpg = to.with_file_name(format!(".{}.cover-tmp.jpg", to.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()));
    let scale = if width > 0 { format!("scale='min({width},iw)':-2") } else { "null".into() };
    // 0:v 去掉 0:V 剩下的就是封面圖
    let args = ["-hide_banner", "-loglevel", "error", "-i", from, "-map", "0:v", "-map", "-0:V", "-frames:v", "1", "-vf", &scale, "-q:v", "3", "-y"];
    let mut args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    args.push(jpg.display().to_string());
    let r = crate::process::run(ffmpeg, &args, Duration::from_secs(30)).await;
    match std::fs::metadata(&jpg) {
        Ok(m) if r.code == 0 && m.len() > 0 => Some((jpg, m.len())),
        _ => {
            let _ = std::fs::remove_file(&jpg);
            None
        }
    }
}

/// 用 FFmpeg 改寫影片（只複製串流）：args 拿暫存檔路徑產生參數，先寫到暫存檔，成功才換掉原檔；
/// 原檔的修改時間保留（清單依時間排序，不要跳到最上面）。失敗時回傳給使用者看的原因
pub(crate) async fn rewrite_in_place(ffmpeg: &Path, video: &str, args: impl FnOnce(&str) -> Vec<String>) -> std::result::Result<(), String> {
    let v = Path::new(video);
    let ext = v.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "mp4".into());
    let tmp = v.with_file_name(format!(".{}.rewrite-tmp.{ext}", v.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()));
    let r = crate::process::run(ffmpeg, &args(&tmp.display().to_string()), Duration::from_secs(600)).await;
    let ok = r.code == 0 && tokio::fs::metadata(&tmp).await.map(|m| m.len() > 0).unwrap_or(false);
    if !ok {
        let _ = tokio::fs::remove_file(&tmp).await;
        crate::info!("[改寫影片] {video} 失敗：{}", r.stderr.trim());
        return Err(tr!("無法寫進影片（詳見記錄檔）", "Couldn't write it into the video (see the log file)").into());
    }
    let mtime = std::fs::metadata(v).and_then(|m| m.modified()).ok();
    if let Err(e) = tokio::fs::rename(&tmp, v).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        crate::info!("[改寫影片] 換掉 {video} 失敗：{e}");
        return Err(tr!("影片正在使用中（播放中或被其他程式開啟），沒有改到影片", "The video is in use (playing or open in another program), so it wasn't changed").into());
    }
    if let (Some(t), Ok(f)) = (mtime, std::fs::File::options().write(true).open(v)) {
        let _ = f.set_modified(t);
    }
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
    let png = crate::paths::unique_temp("ScreenRecorder-thumb", ".png");
    let data = pm.encode_png().map_err(|e| Error::other(e.to_string()))?;
    tokio::fs::write(&png, data).await.map_err(|e| Error::other(e.to_string()))?;
    let r = embed_png(ffmpeg, video, &png).await;
    let _ = tokio::fs::remove_file(&png).await;
    r.map_err(Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embed_args_copy_streams_and_replace_the_cover() {
        let a = embed_args("in.mp4", "c.png", "out.mp4");
        let s = a.join(" ");
        assert!(s.contains("-map 0:V -map 0:a? -map 0:s? -map 1 -c copy -disposition:v:1 attached_pic"), "{s}");
        assert!(s.ends_with("-y out.mp4"));
    }
}
