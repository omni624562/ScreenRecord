//! 旋轉影片：改檔案裡的旋轉標記（手機直拍的影片就是靠它變直的），只複製串流、不重新壓縮：
//! 幾秒就好、畫質與大小不變，影片資訊、封面、章節、修改時間都留著，轉錯再轉回來就好。
//! 少數舊播放器不看旋轉標記：可以另存一份畫面真的轉好的影片（重新壓縮，見 exporter::start_upright）。

use crate::{tr, Error, Result};
use std::path::Path;
use std::time::Duration;

/// 往哪邊轉
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Turn {
    /// 向左（逆時針）90°
    Left,
    /// 向右（順時針）90°
    Right,
    Half,
}

impl Turn {
    /// 逆時針的度數（FFmpeg 的旋轉標記用逆時針）
    fn ccw(self) -> i32 {
        match self {
            Turn::Left => 90,
            Turn::Right => -90,
            Turn::Half => 180,
        }
    }
}

/// 目前的旋轉標記（FFmpeg 顯示的度數，逆時針為正）再轉一次後的度數：-90、0、90、180 其中之一
pub fn next_rotation(current: f64, turn: Turn) -> i32 {
    let cur = ((current / 90.0).round() as i32) * 90;
    let r = (cur + turn.ccw()).rem_euclid(360);
    if r == 270 {
        -90
    } else {
        r
    }
}

/// 只換旋轉標記（第一條畫面串流）；其他串流、影片資訊、章節照搬（跟 video_tags::write_args 一樣）
pub fn tag_args(video: &str, rotation: i32, out: &str) -> Vec<String> {
    let mut a: Vec<String> = ["-hide_banner", "-nostats", "-loglevel", "error", "-display_rotation:v:0"].iter().map(|s| s.to_string()).collect();
    a.push(rotation.to_string());
    a.extend(
        ["-i", video, "-map", "0:v?", "-map", "0:a?", "-map", "0:s?", "-c", "copy", "-map_metadata", "0", "-map_chapters", "0", "-movflags", "+faststart", "-y", out].iter().map(|s| s.to_string()),
    );
    a
}

/// 讀影片的旋轉標記（逆時針度數；沒有標記時 0）
pub async fn rotation_of(ffmpeg: &Path, video: &str) -> Result<f64> {
    let r = crate::process::run(ffmpeg, &["-hide_banner".to_string(), "-i".into(), video.to_string()], Duration::from_secs(30)).await;
    crate::args::parse_rotation(&r.stderr).ok_or_else(|| Error::other(tr!("讀不到這支影片的畫面", "Couldn't read this video's picture")))
}

/// 旋轉原檔（先寫到暫存檔，成功才換掉；修改時間保留）
pub async fn rotate_in_place(ffmpeg: Option<&Path>, video: &str, turn: Turn) -> Result<()> {
    let ff = ffmpeg.ok_or_else(|| Error::config(tr!("找不到 FFmpeg", "FFmpeg not found")))?;
    if !crate::video_tags::can_tag(video) {
        return Err(Error::config(tr!("只有 MP4 影片可以旋轉", "Only MP4 videos can be rotated")));
    }
    let to = next_rotation(rotation_of(ff, video).await?, turn);
    crate::cover::rewrite_in_place(ff, video, |tmp| tag_args(video, to, tmp)).await.map_err(Error::other)?;
    crate::info!("[旋轉] {video}：旋轉標記改成 {to}°");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turns_add_up_and_wrap() {
        assert_eq!(next_rotation(0.0, Turn::Left), 90);
        assert_eq!(next_rotation(0.0, Turn::Right), -90);
        assert_eq!(next_rotation(0.0, Turn::Half), 180);
        // 手機直拍（-90）向左轉回去就沒有標記了
        assert_eq!(next_rotation(-90.0, Turn::Left), 0);
        assert_eq!(next_rotation(90.0, Turn::Left), 180);
        assert_eq!(next_rotation(-180.0, Turn::Left), -90);
        assert_eq!(next_rotation(89.6, Turn::Right), 0);
        let a = tag_args("in.mp4", -90, "out.mp4").join(" ");
        assert!(a.contains("-display_rotation:v:0 -90 -i in.mp4 -map 0:v? -map 0:a? -map 0:s? -c copy -map_metadata 0 -map_chapters 0"), "{a}");
    }
}
