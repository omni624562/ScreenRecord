//! 影片章節：錄影時加的標記在存檔時寫成 MP4 章節（播放器可以直接跳章節），
//! 製作加速版、剪輯時換算成新的時間；也能複製成 YouTube 說明欄的章節文字。

use crate::tr;
use crate::types::Chapter;
use regex::Regex;
use std::sync::LazyLock;

/// 離開頭或結尾這麼近的標記不另開章節（秒）
const EDGE: f64 = 0.5;

/// 標記 → 章節：開頭一章，之後每個標記開一章（「標記 1」「標記 2」…）
pub fn from_markers(markers: &[f64], duration: f64) -> Vec<Chapter> {
    let mut times: Vec<f64> = markers.iter().copied().filter(|t| t.is_finite() && *t < duration - EDGE).collect();
    times.sort_by(f64::total_cmp);
    if times.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    if times[0] > EDGE {
        out.push(Chapter { start: 0.0, title: tr!("開頭", "Start").into() });
    }
    for (i, t) in times.iter().enumerate() {
        let start = if *t <= EDGE { 0.0 } else { *t };
        if out.last().is_some_and(|c: &Chapter| start - c.start < EDGE) {
            continue;
        }
        out.push(Chapter { start, title: crate::trf!("標記 {}", "Marker {}", i + 1) });
    }
    out
}

/// 寫給 FFmpeg 的章節檔（FFMETADATA1，毫秒）
pub fn ffmetadata(chapters: &[Chapter], duration: f64) -> String {
    let mut s = String::from(";FFMETADATA1\n");
    for (i, c) in chapters.iter().enumerate() {
        let end = chapters.get(i + 1).map(|n| n.start).unwrap_or(duration).max(c.start + 0.001);
        s.push_str(&format!("[CHAPTER]\nTIMEBASE=1/1000\nSTART={}\nEND={}\ntitle={}\n", (c.start * 1000.0).round() as u64, (end * 1000.0).round() as u64, escape(&c.title)));
    }
    s
}

/// FFMETADATA 的特殊字元前面加反斜線
fn escape(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        match ch {
            '=' | ';' | '#' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            '\n' | '\r' => out.push(' '),
            _ => out.push(ch),
        }
    }
    out
}

/// 加速版：時間除以倍率
pub fn scale(chapters: &[Chapter], speed: f64) -> Vec<Chapter> {
    chapters.iter().map(|c| Chapter { start: c.start / speed.max(1.0), title: c.title.clone() }).collect()
}

/// 剪輯後：換成剪輯後的時間；開頭落在刪掉的片段裡時，改從下一個保留的片段開始（重疊的合併）
pub fn map_cut(chapters: &[Chapter], keep: &[(f64, f64, u32)]) -> Vec<Chapter> {
    let mut out: Vec<Chapter> = Vec::new();
    for c in chapters {
        let t = crate::zoom::map_time(keep, c.start).or_else(|| keep.iter().find(|k| k.0 > c.start).and_then(|k| crate::zoom::map_time(keep, k.0)));
        let Some(t) = t else { continue };
        match out.last_mut() {
            // 同一個時間點：後面的章節取代前面的（例如開頭被刪掉）
            Some(last) if (t - last.start).abs() < EDGE => *last = Chapter { start: last.start, title: c.title.clone() },
            _ => out.push(Chapter { start: t, title: c.title.clone() }),
        }
    }
    if let Some(first) = out.first_mut() {
        first.start = 0.0;
    }
    out
}

/// YouTube 說明欄的章節：第一行從 0:00 開始；一小時以上用 h:mm:ss
pub fn youtube_text(chapters: &[Chapter], duration: f64) -> String {
    let long = duration >= 3600.0;
    let stamp = |t: f64| {
        let s = t.max(0.0).floor() as u64;
        if long {
            format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
        } else {
            format!("{}:{:02}", s / 60, s % 60)
        }
    };
    let mut lines = Vec::new();
    if chapters.first().is_none_or(|c| c.start >= 1.0) {
        lines.push(format!("{} {}", stamp(0.0), tr!("開頭", "Start")));
    }
    for c in chapters {
        lines.push(format!("{} {}", stamp(c.start), c.title));
    }
    lines.join("\n")
}

static CHAPTER_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"Chapter #\d+:\d+: start (-?\d+(?:\.\d+)?)").unwrap());
static TITLE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*title\s*:\s?(.*)$").unwrap());

/// 讀 `ffmpeg -i` 的 stderr 裡的章節
pub fn parse(stderr: &str) -> Vec<Chapter> {
    let mut out: Vec<Chapter> = Vec::new();
    let mut in_chapter = false;
    for line in stderr.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(c) = CHAPTER_RE.captures(line) {
            out.push(Chapter { start: c[1].parse().unwrap_or(0.0), title: String::new() });
            in_chapter = true;
        } else if line.trim_start().starts_with("Stream #") || line.trim_start().starts_with("Input #") || line.trim_start().starts_with("Output #") {
            in_chapter = false;
        } else if in_chapter {
            if let (Some(t), Some(c)) = (TITLE_RE.captures(line), out.last_mut()) {
                if c.title.is_empty() {
                    c.title = t[1].trim().to_string();
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(c: &[Chapter]) -> Vec<(f64, &str)> {
        c.iter().map(|c| (c.start, c.title.as_str())).collect()
    }

    #[test]
    fn markers_become_chapters() {
        assert!(from_markers(&[], 60.0).is_empty());
        // 開頭一章；太靠近結尾的標記不算
        assert_eq!(titles(&from_markers(&[30.0, 12.5, 59.8], 60.0)), vec![(0.0, "開頭"), (12.5, "標記 1"), (30.0, "標記 2")]);
        // 一開始就加的標記取代開頭
        assert_eq!(titles(&from_markers(&[0.2, 10.0], 60.0)), vec![(0.0, "標記 1"), (10.0, "標記 2")]);
        let meta = ffmetadata(&from_markers(&[12.5], 60.0), 60.0);
        assert_eq!(meta, ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=12500\ntitle=開頭\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=12500\nEND=60000\ntitle=標記 1\n");
        assert_eq!(escape("a=b;c#d\\e\nf"), "a\\=b\\;c\\#d\\\\e f");
    }

    #[test]
    fn chapters_follow_speed_and_cuts() {
        let c = vec![Chapter { start: 0.0, title: "A".into() }, Chapter { start: 20.0, title: "B".into() }, Chapter { start: 45.0, title: "C".into() }];
        assert_eq!(titles(&scale(&c, 4.0)), vec![(0.0, "A"), (5.0, "B"), (11.25, "C")]);
        // 保留 0–10、30–60：B 落在刪掉的片段裡，改從 30（剪輯後 10）開始；C 在 45 → 25
        assert_eq!(titles(&map_cut(&c, &[(0.0, 10.0, 1), (30.0, 60.0, 1)])), vec![(0.0, "A"), (10.0, "B"), (25.0, "C")]);
        // 開頭被剪掉：A 從第一個保留的片段開始；加速的片段時間變短（25–45 以 2 倍播放只佔 10 秒）
        assert_eq!(titles(&map_cut(&c, &[(15.0, 25.0, 1), (25.0, 45.0, 2), (45.0, 50.0, 1)])), vec![(0.0, "A"), (5.0, "B"), (20.0, "C")]);
        // 兩章落在同一個地方：留後面那一章
        assert_eq!(titles(&map_cut(&c, &[(30.0, 60.0, 1)])), vec![(0.0, "B"), (15.0, "C")]);
    }

    #[test]
    fn youtube_and_parse() {
        let c = vec![Chapter { start: 0.0, title: "開頭".into() }, Chapter { start: 83.4, title: "標記 1".into() }];
        assert_eq!(youtube_text(&c, 200.0), "0:00 開頭\n1:23 標記 1");
        assert_eq!(youtube_text(&c[1..], 4000.0), "0:00:00 開頭\n0:01:23 標記 1");
        let stderr = "Input #0, mov,mp4, from 'a.mp4':\n  Duration: 00:01:00.00, start: 0.000000, bitrate: 1 kb/s\n  Chapters:\n    Chapter #0:0: start 0.000000, end 12.500000\r\n      Metadata:\n        title           : 開頭\n    Chapter #0:1: start 12.500000, end 60.000000\n      Metadata:\n        title           : 標記 1\n  Stream #0:0[0x1](und): Video: h264\n      Metadata:\n        title           : 不是章節\n";
        assert_eq!(titles(&parse(stderr)), vec![(0.0, "開頭"), (12.5, "標記 1")]);
    }
}
