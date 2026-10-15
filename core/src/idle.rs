//! 找出沒動靜的片段：畫面不動（FFmpeg freezedetect）而且沒有聲音（silencedetect）的地方，
//! 讓剪輯時按一下就刪掉等待、發呆的時間。

use crate::edit::Range;
use crate::tr;
use std::path::Path;
use std::time::Duration;

/// 不動又沒聲音超過這麼久才算
pub const MIN_IDLE: f64 = 2.0;
/// 頭尾各留一點，剪接處才不會太突然
const PAD: f64 = 0.3;

/// 分析用的 FFmpeg 參數（畫面先縮小，加快速度；輸出丟掉）
pub fn detect_args(input: &str, has_audio: bool) -> Vec<String> {
    let mut a: Vec<String> = ["-hide_banner", "-nostats", "-loglevel", "info", "-i", input].iter().map(|s| s.to_string()).collect();
    a.extend(["-map".into(), "0:v:0".into(), "-vf".into(), format!("scale=640:-2,freezedetect=n=-60dB:d={MIN_IDLE}")]);
    if has_audio {
        a.extend(["-map".into(), "0:a:0".into(), "-af".into(), format!("silencedetect=n=-45dB:d={MIN_IDLE}")]);
    } else {
        a.push("-an".into());
    }
    a.extend(["-f".into(), "null".into(), "-".into()]);
    a
}

/// 從 FFmpeg 的輸出讀出 (畫面不動的片段, 沒有聲音的片段)；沒結束的算到影片結尾
pub fn parse(stderr: &str, duration: f64) -> (Vec<Range>, Vec<Range>) {
    let num = |line: &str, key: &str| -> Option<f64> {
        let rest = &line[line.find(key)? + key.len()..];
        rest.trim_start().split(|c: char| c.is_whitespace() || c == '|').next()?.parse().ok()
    };
    let (mut freeze, mut silence) = (vec![], vec![]);
    let (mut f0, mut s0): (Option<f64>, Option<f64>) = (None, None);
    for line in stderr.lines() {
        if let Some(t) = num(line, "freeze_start:") {
            f0 = Some(t);
        } else if let Some(t) = num(line, "freeze_end:") {
            if let Some(a) = f0.take() {
                freeze.push((a, t));
            }
        } else if let Some(t) = num(line, "silence_start:") {
            s0 = Some(t);
        } else if let Some(t) = num(line, "silence_end:") {
            if let Some(a) = s0.take() {
                silence.push((a, t));
            }
        }
    }
    if let Some(a) = f0 {
        freeze.push((a, duration));
    }
    if let Some(a) = s0 {
        silence.push((a, duration));
    }
    (freeze, silence)
}

/// 畫面不動、而且（有聲音時）也沒聲音的片段，頭尾各留一點；太短的不算
pub fn idle_ranges(freeze: &[Range], silence: Option<&[Range]>, duration: f64) -> Vec<Range> {
    let both: Vec<Range> = match silence {
        Some(sil) => freeze.iter().flat_map(|&(a, b)| sil.iter().filter_map(move |&(c, d)| (a.max(c) < b.min(d)).then_some((a.max(c), b.min(d))))).collect(),
        None => freeze.to_vec(),
    };
    both.into_iter()
        .map(|(a, b)| ((a + PAD).max(0.0), (b - PAD).min(duration)))
        .filter(|&(a, b)| b - a >= MIN_IDLE - PAD * 2.0)
        .map(|(a, b)| ((a * 1000.0).round() / 1000.0, (b * 1000.0).round() / 1000.0))
        .collect()
}

/// 沒說話（但畫面在動）超過這麼久才算：打字、捲動、等待讀取，適合加速帶過
pub const MIN_QUIET: f64 = 3.0;

/// a 扣掉 b 的部分（兩邊都由小到大、不重疊）
fn subtract(a: &[Range], b: &[Range]) -> Vec<Range> {
    let mut out = vec![];
    for &(mut x, y) in a {
        for &(c, d) in b {
            if d <= x || c >= y {
                continue;
            }
            if c > x {
                out.push((x, c));
            }
            x = x.max(d);
        }
        if y > x {
            out.push((x, y));
        }
    }
    out
}

/// 沒有聲音、但畫面在動的片段（例如打字、捲動、等待讀取），頭尾各留一點；太短的不算
pub fn quiet_ranges(freeze: &[Range], silence: &[Range], duration: f64) -> Vec<Range> {
    subtract(silence, freeze)
        .into_iter()
        .map(|(a, b)| ((a + PAD).max(0.0), (b - PAD).min(duration)))
        .filter(|&(a, b)| b - a >= MIN_QUIET - PAD * 2.0)
        .map(|(a, b)| ((a * 1000.0).round() / 1000.0, (b * 1000.0).round() / 1000.0))
        .collect()
}

/// 分析的結果
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Found {
    /// 畫面不動、也沒聲音：可以刪掉
    pub idle: Vec<Range>,
    /// 沒說話、畫面在動：可以加速帶過（沒有聲音的影片是空的）
    pub quiet: Vec<Range>,
}

/// 分析整支影片：沒動靜的片段與沒說話的片段
pub async fn analyze(ffmpeg: &Path, input: &str, duration: f64, has_audio: bool) -> crate::Result<Found> {
    let args = detect_args(input, has_audio);
    // 解碼整支影片：給到影片長度的兩倍再加一分鐘
    let r = crate::process::run(ffmpeg, &args, Duration::from_secs_f64(duration.max(0.0) * 2.0 + 60.0)).await;
    if r.timed_out {
        return Err(crate::Error::other(tr!("分析太久，已停止", "Analysis took too long and was stopped")));
    }
    if r.code != 0 {
        crate::info!("[剪輯] 找沒動靜的片段失敗：{}", r.stderr.lines().rev().take(3).collect::<Vec<_>>().join(" / "));
        return Err(crate::Error::other(tr!("無法分析這支影片，詳見記錄檔", "Couldn't analyze this video. See the log file for details.")));
    }
    let (freeze, silence) = parse(&r.stderr, duration);
    let found = Found {
        idle: idle_ranges(&freeze, has_audio.then_some(&silence[..]), duration),
        quiet: if has_audio { quiet_ranges(&freeze, &silence, duration) } else { vec![] },
    };
    crate::info!("[剪輯] {input}：畫面不動 {} 段、沒聲音 {} 段，可刪 {} 段、可加速 {} 段", freeze.len(), silence.len(), found.idle.len(), found.quiet.len());
    Ok(found)
}

/// 分析整支影片，回傳沒動靜的片段
pub async fn find(ffmpeg: &Path, input: &str, duration: f64, has_audio: bool) -> crate::Result<Vec<Range>> {
    Ok(analyze(ffmpeg, input, duration, has_audio).await?.idle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_detections_and_keeps_both_still_and_silent() {
        let log = "\
[freezedetect @ 0x5] lavfi.freezedetect.freeze_start: 2.5
[freezedetect @ 0x5] lavfi.freezedetect.freeze_duration: 5.5
[freezedetect @ 0x5] lavfi.freezedetect.freeze_end: 8
[silencedetect @ 0x6] silence_start: 3
[silencedetect @ 0x6] silence_end: 10 | silence_duration: 7
[freezedetect @ 0x5] lavfi.freezedetect.freeze_start: 20.04
[silencedetect @ 0x6] silence_start: 19.5
";
        let (f, s) = parse(log, 30.0);
        assert_eq!(f, vec![(2.5, 8.0), (20.04, 30.0)]);
        assert_eq!(s, vec![(3.0, 10.0), (19.5, 30.0)]);
        // 兩者重疊的部分，頭尾各留 0.3 秒
        assert_eq!(idle_ranges(&f, Some(&s), 30.0), vec![(3.3, 7.7), (20.34, 29.7)]);
        // 沒聲音的影片只看畫面
        assert_eq!(idle_ranges(&f, None, 30.0), vec![(2.8, 7.7), (20.34, 29.7)]);
        // 重疊太短的不算
        assert!(idle_ranges(&[(0.0, 5.0)], Some(&[(4.0, 9.0)]), 30.0).is_empty());
        let a = detect_args("in.mp4", true).join(" ");
        assert!(a.contains("freezedetect=n=-60dB:d=2") && a.contains("silencedetect=n=-45dB:d=2") && a.ends_with("-f null -"));
        assert!(detect_args("in.mp4", false).contains(&"-an".to_string()));
    }

    #[test]
    fn quiet_is_silence_where_the_picture_moves() {
        // 沒聲音 2–12 秒，其中 5–8 秒畫面也不動 → 沒說話但畫面在動：2–5、8–12（頭尾各留 0.3 秒）
        let q = quiet_ranges(&[(5.0, 8.0)], &[(2.0, 12.0)], 20.0);
        assert_eq!(q, vec![(2.3, 4.7), (8.3, 11.7)]);
        // 太短的不算：沒聲音 2–4 秒
        assert!(quiet_ranges(&[], &[(2.0, 4.0)], 20.0).is_empty());
        assert_eq!(subtract(&[(0.0, 10.0)], &[(2.0, 3.0), (5.0, 6.0)]), vec![(0.0, 2.0), (3.0, 5.0), (6.0, 10.0)]);
        assert!(quiet_ranges(&[], &[], 10.0).is_empty());
    }
}
