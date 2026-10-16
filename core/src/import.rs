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
