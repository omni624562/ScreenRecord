//! 匯入其他影片：手機、OBS、其他錄影軟體錄的 MOV、MKV、AVI、WMV、WebM… 轉成 MP4，才能剪輯、製作加速版等。
//! 已經是 H.264 / HEVC 的直接換成 MP4 外殼（不重新壓縮，很快、畫質不變）；其他的用設定好的編碼器重新壓縮。

use crate::args::EncoderSpec;
use std::path::Path;

/// 認得的其他影片格式（小寫、不含點）
pub const OTHER_VIDEO_EXTS: [&str; 11] = ["mov", "mkv", "avi", "wmv", "webm", "m4v", "flv", "mpg", "mpeg", "ts", "3gp"];

fn ext_of(name: &str) -> String {
    Path::new(name).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default()
}

/// 要先轉成 MP4 的影片（MOV、MKV…）
pub fn is_other_video(name: &str) -> bool {
    OTHER_VIDEO_EXTS.contains(&ext_of(name).as_str())
}

/// 可以匯入的影片（MP4 或其他格式）
pub fn is_importable(name: &str) -> bool {
    ext_of(name) == "mp4" || is_other_video(name)
}

/// 格式的簡稱（清單上標示用），例如「MOV」
pub fn format_label(name: &str) -> String {
    ext_of(name).to_ascii_uppercase()
}

/// 從 `ffmpeg -i` 的輸出讀出第一個畫面串流的編碼（封面圖不算），例如 "h264"、"hevc"、"vp9"
pub fn video_codec(ffmpeg_info: &str) -> Option<String> {
    ffmpeg_info.lines().filter(|l| l.contains("Stream #") && l.contains(": Video: ") && !l.contains("(attached pic)")).find_map(|l| {
        let rest = l.split(": Video: ").nth(1)?;
        Some(rest.split([' ', ',', '(']).next()?.trim().to_ascii_lowercase()).filter(|c| !c.is_empty())
    })
}

/// 合併時判斷能不能直接接起來（不重新壓縮）：從 `ffmpeg -i` 的輸出取出每個串流的格式
/// （編碼、像素格式、尺寸、時間單位、聲音的取樣率與聲道）和畫面的旋轉；位元率、張數會隨內容變，不算。
/// 其他串流（章節、資料）只記種類：concat 依串流的順序對應，順序也要一樣。每支影片算出來一樣才能接；沒有畫面時 None
pub fn stream_signature(ffmpeg_info: &str) -> Option<String> {
    let mut parts = vec![];
    let mut has_video = false;
    for l in ffmpeg_info.lines().map(str::trim) {
        if l.starts_with("Stream #") {
            let Some((kind, rest)) = ["Video", "Audio", "Data", "Subtitle", "Attachment"].iter().find_map(|k| l.split_once(&format!(": {k}: ")).map(|(_, r)| (*k, r))) else { continue };
            match kind {
                "Video" if l.contains("(attached pic)") => continue,
                "Video" | "Audio" => {
                    has_video |= kind == "Video";
                    let keep: Vec<&str> = rest.split(", ").map(|t| t.trim_end_matches(" (default)").trim()).filter(|t| !t.ends_with("kb/s") && !t.ends_with(" fps") && !t.ends_with(" tbr")).collect();
                    parts.push(format!("{kind}: {}", keep.join(", ")));
                }
                _ => parts.push(kind.to_string()),
            }
        } else if l.contains("rotation of") {
            parts.push(l.to_string());
        }
    }
    has_video.then(|| parts.join(" | "))
}

/// MP4 可以直接裝、Windows 和手機都播得了的畫面編碼：不用重新壓縮
pub fn can_copy_video(codec: Option<&str>) -> bool {
    matches!(codec, Some("h264") | Some("hevc"))
}

/// 轉成 MP4 的參數：copy = 畫面直接搬過去；聲音一律轉成 AAC（很快，也處理 PCM、Opus 等 MP4 不常見的格式）
pub fn convert_args(src: &str, out: &str, copy: bool, enc: &EncoderSpec) -> Vec<String> {
    let mut a: Vec<String> = ["-hide_banner", "-nostats", "-loglevel", "error", "-i", src, "-map", "0:V:0", "-map", "0:a:0?", "-sn", "-dn"].iter().map(|s| s.to_string()).collect();
    if copy {
        a.extend(["-c:v", "copy"].iter().map(|s| s.to_string()));
    } else {
        // 寬高要是偶數；畫面轉正（手機直拍的影片）由 FFmpeg 自動處理
        a.extend(["-vf".to_string(), format!("scale=trunc(iw/2)*2:trunc(ih/2)*2,format={}", enc.pix_fmt)]);
        a.extend(enc.offline());
    }
    a.extend(
        ["-c:a", "aac", "-b:a", "160k", "-map_metadata", "0", "-map_chapters", "0", "-movflags", "+faststart", "-progress", "pipe:1", "-stats_period", "0.5", "-y", out].iter().map(|s| s.to_string()),
    );
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_signature_ignores_bitrate_and_fps_but_not_format() {
        let a = "  Stream #0:0[0x1](eng): Video: hevc (Main) (hvc1 / 0x31637668), yuv420p(tv, bt709), 1920x1080, 20046 kb/s, 30.01 fps, 30 tbr, 90k tbn (default)
      displaymatrix: rotation of -90.00 degrees
  Stream #0:1[0x2](eng): Audio: aac (LC) (mp4a / 0x6134706D), 48000 Hz, stereo, fltp, 256 kb/s (default)
  Stream #0:2[0x3](eng): Data: none (mett / 0x7474656D), 0 kb/s";
        let b = a.replace("20046 kb/s", "18000 kb/s").replace("30.01 fps", "29.97 fps").replace("256 kb/s", "250 kb/s");
        assert_eq!(stream_signature(a), stream_signature(&b));
        assert!(stream_signature(a).unwrap().contains("rotation of -90.00"));
        // 尺寸、旋轉、聲音不同就不能直接接
        assert_ne!(stream_signature(a), stream_signature(&a.replace("1920x1080", "1280x720")));
        assert_ne!(stream_signature(a), stream_signature(&a.replace("-90.00", "90.00")));
        assert_ne!(stream_signature(a), stream_signature(&a.replace("48000 Hz", "44100 Hz")));
        // 封面圖不算；沒有畫面時 None
        let cover = format!("{a}\n  Stream #0:3: Video: png, rgb24(pc), 640x360, 90k tbn (attached pic)");
        assert_eq!(stream_signature(&cover), stream_signature(a));
        assert_eq!(stream_signature("  Stream #0:0: Audio: aac (LC), 48000 Hz, stereo, fltp, 128 kb/s"), None);
    }
    use crate::args::ENCODERS;

    #[test]
    fn formats_and_codecs() {
        assert!(is_other_video("IMG_0012.MOV") && is_other_video("2026-10-16 10-00-00.mkv"));
        assert!(!is_other_video("Rec.mp4") && is_importable("Rec.mp4") && !is_importable("a.png") && !is_importable("mov"));
        assert_eq!(format_label("a.Mkv"), "MKV");
        let info = "  Stream #0:0[0x1](und): Video: hevc (Main 10) (hvc1 / 0x31637668), yuv420p10le, 1920x1080\n  Stream #0:1: Audio: aac\n";
        assert_eq!(video_codec(info).as_deref(), Some("hevc"));
        let cover = "  Stream #0:0: Video: mjpeg (Baseline), yuvj420p, 300x300 (attached pic)\n  Stream #0:1: Video: vp9, yuv420p, 1280x720\n";
        assert_eq!(video_codec(cover).as_deref(), Some("vp9"));
        assert!(can_copy_video(Some("h264")) && !can_copy_video(Some("vp9")) && !can_copy_video(None));
    }

    #[test]
    fn convert_copies_or_reencodes() {
        let copy = convert_args("in.mov", "out.mp4", true, &ENCODERS[0]).join(" ");
        assert!(copy.contains("-map 0:V:0 -map 0:a:0? -sn -dn -c:v copy -c:a aac"), "{copy}");
        assert!(copy.ends_with("-y out.mp4"));
        let enc = convert_args("in.webm", "out.mp4", false, &ENCODERS[0]).join(" ");
        assert!(enc.contains("-vf scale=trunc(iw/2)*2:trunc(ih/2)*2,format=yuv420p -c:v libx264"), "{enc}");
        assert!(!enc.contains("-c:v copy"));
    }
}
