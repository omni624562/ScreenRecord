//! 影片章節：錄影時加的標記在存檔時寫成 MP4 章節（播放器可以直接跳章節），
//! 製作加速版、剪輯時換算成新的時間；也能複製成 YouTube 說明欄的章節文字。

use crate::tr;
use crate::types::Chapter;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

/// 剪輯視窗裡的標記：時間（原影片的秒數）與名稱（空白 = 「標記 N」）
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Marker {
    pub t: f64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
}

/// 離開頭或結尾這麼近的標記不另開章節（秒）
const EDGE: f64 = 0.5;

/// 標記 → 章節：開頭一章，之後每個標記開一章（「標記 1」「標記 2」…）
pub fn from_markers(markers: &[f64], duration: f64) -> Vec<Chapter> {
    let named: Vec<Marker> = markers.iter().map(|&t| Marker { t, name: String::new() }).collect();
    from_named(&named, duration)
}

/// 有名稱的標記 → 章節：和 from_markers 相同，有名稱的用名稱當章節標題（沒有的照舊是「標記 N」，N 依時間排序）
pub fn from_named(markers: &[Marker], duration: f64) -> Vec<Chapter> {
    let mut list: Vec<&Marker> = markers.iter().filter(|m| m.t.is_finite() && m.t < duration - EDGE).collect();
    list.sort_by(|a, b| a.t.total_cmp(&b.t));
    if list.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    if list[0].t > EDGE {
        out.push(Chapter { start: 0.0, title: tr!("開頭", "Start").into() });
    }
    for (i, m) in list.iter().enumerate() {
        let start = if m.t <= EDGE { 0.0 } else { m.t };
        if out.last().is_some_and(|c: &Chapter| start - c.start < EDGE) {
            continue;
        }
        let title = if m.name.trim().is_empty() { crate::trf!("標記 {}", "Marker {}", i + 1) } else { m.name.trim().to_string() };
        out.push(Chapter { start, title });
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
pub fn map_cut(chapters: &[Chapter], keep: &[crate::edit::Part]) -> Vec<Chapter> {
    let mut out: Vec<Chapter> = Vec::new();
    for c in chapters {
        let t = crate::zoom::map_time(keep, c.start).or_else(|| keep.iter().find(|k| !k.is_hold() && k.a > c.start).and_then(|k| crate::zoom::map_time(keep, k.a)));
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

/// 每章至少這麼長（秒）
const SUB_MIN_CHAPTER: f64 = 30.0;
/// 字幕間停頓至少這麼久才算換段（秒）
const SUB_MIN_GAP: f64 = 1.0;
/// 章節標題最多幾個字
const SUB_TITLE_CHARS: usize = 24;

/// 字幕 → 章節標記：在講話停頓最久的地方切開（每章至少 30 秒，大約每分鐘最多一章），
/// 標題用那一章第一句字幕。影片太短或沒有停頓時回傳空的
pub fn from_subtitles(cues: &[crate::subtitles::Cue], duration: f64) -> Vec<Marker> {
    let mut cues: Vec<&crate::subtitles::Cue> = cues.iter().filter(|c| !c.text.trim().is_empty() && c.start < duration).collect();
    cues.sort_by(|a, b| a.start.total_cmp(&b.start));
    if cues.is_empty() || duration < SUB_MIN_CHAPTER * 2.0 {
        return vec![];
    }
    let max_chapters = ((duration / 60.0).ceil() as usize).clamp(2, 20);
    // 候選切點：每段停頓後的下一句開頭，停頓越久越優先
    let mut gaps: Vec<(f64, usize)> = (1..cues.len()).map(|i| (cues[i].start - cues[i - 1].end, i)).filter(|(g, _)| *g >= SUB_MIN_GAP).collect();
    gaps.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut picked: Vec<usize> = vec![];
    for (_, i) in gaps {
        if picked.len() + 1 >= max_chapters {
            break;
        }
        let t = cues[i].start;
        let far = t >= SUB_MIN_CHAPTER && duration - t >= SUB_MIN_CHAPTER && picked.iter().all(|&j| (cues[j].start - t).abs() >= SUB_MIN_CHAPTER);
        if far {
            picked.push(i);
        }
    }
    if picked.is_empty() {
        return vec![];
    }
    picked.sort();
    let title = |c: &crate::subtitles::Cue| {
        let flat: String = c.text.split_whitespace().collect::<Vec<_>>().join(" ");
        let flat = flat.trim_end_matches(['，', '。', ',', '.', '、', '！', '？', '!', '?']).to_string();
        if flat.chars().count() > SUB_TITLE_CHARS {
            format!("{}…", flat.chars().take(SUB_TITLE_CHARS).collect::<String>())
        } else {
            flat
        }
    };
    let mut out = vec![Marker { t: 0.0, name: title(cues[0]) }];
    // 停頓的中間開始新的一章，跳過去時不會切掉第一個字
    out.extend(picked.into_iter().map(|i| Marker { t: ((cues[i - 1].end + cues[i].start) / 2.0).max(cues[i].start - 0.5), name: title(cues[i]) }));
    out
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
    fn named_markers() {
        let m = |t: f64, name: &str| Marker { t, name: name.into() };
        // 有名稱的用名稱，沒有的照舊叫「標記 N」（N 依時間排序）
        assert_eq!(titles(&from_named(&[m(30.0, ""), m(12.5, "登入"), m(59.8, "太靠近結尾")], 60.0)), vec![(0.0, "開頭"), (12.5, "登入"), (30.0, "標記 2")]);
        assert!(from_named(&[], 60.0).is_empty());
    }

    #[test]
    fn chapters_follow_speed_and_cuts() {
        let c = vec![Chapter { start: 0.0, title: "A".into() }, Chapter { start: 20.0, title: "B".into() }, Chapter { start: 45.0, title: "C".into() }];
        assert_eq!(titles(&scale(&c, 4.0)), vec![(0.0, "A"), (5.0, "B"), (11.25, "C")]);
        // 保留 0–10、30–60：B 落在刪掉的片段裡，改從 30（剪輯後 10）開始；C 在 45 → 25
        use crate::edit::Part as P;
        assert_eq!(titles(&map_cut(&c, &[P::new(0.0, 10.0, 1.0), P::new(30.0, 60.0, 1.0)])), vec![(0.0, "A"), (10.0, "B"), (25.0, "C")]);
        // 開頭被剪掉：A 從第一個保留的片段開始；加速的片段時間變短（25–45 以 2 倍播放只佔 10 秒）
        assert_eq!(titles(&map_cut(&c, &[P::new(15.0, 25.0, 1.0), P::new(25.0, 45.0, 2.0), P::new(45.0, 50.0, 1.0)])), vec![(0.0, "A"), (5.0, "B"), (20.0, "C")]);
        // 兩章落在同一個地方：留後面那一章
        assert_eq!(titles(&map_cut(&c, &[P::new(30.0, 60.0, 1.0)])), vec![(0.0, "B"), (15.0, "C")]);
    }

    #[test]
    fn youtube_and_parse() {
        let c = vec![Chapter { start: 0.0, title: "開頭".into() }, Chapter { start: 83.4, title: "標記 1".into() }];
        assert_eq!(youtube_text(&c, 200.0), "0:00 開頭\n1:23 標記 1");
        assert_eq!(youtube_text(&c[1..], 4000.0), "0:00:00 開頭\n0:01:23 標記 1");
        let stderr = "Input #0, mov,mp4, from 'a.mp4':\n  Duration: 00:01:00.00, start: 0.000000, bitrate: 1 kb/s\n  Chapters:\n    Chapter #0:0: start 0.000000, end 12.500000\r\n      Metadata:\n        title           : 開頭\n    Chapter #0:1: start 12.500000, end 60.000000\n      Metadata:\n        title           : 標記 1\n  Stream #0:0[0x1](und): Video: h264\n      Metadata:\n        title           : 不是章節\n";
        assert_eq!(titles(&parse(stderr)), vec![(0.0, "開頭"), (12.5, "標記 1")]);
    }

    #[test]
    fn chapters_from_subtitles_split_at_longest_pauses() {
        use crate::subtitles::Cue;
        let cue = |a: f64, b: f64, t: &str| Cue { start: a, end: b, text: t.into() };
        // 三分鐘：每 5 秒一句；第 12、25 句之後停頓 4 秒，第 6 句之後停頓 1.5 秒（比較短，章數夠了就不切）
        let mut cues = vec![];
        let mut t: f64 = 0.0;
        for i in 0..40 {
            let name = if i == 0 { "大家好，今天介紹 設定\n畫面。".to_string() } else { format!("第 {i} 句") };
            cues.push(cue(t, t + 4.5, &name));
            t += 4.5
                + if i == 6 {
                    1.5
                } else if i == 12 || i == 25 {
                    4.0
                } else {
                    0.5
                };
        }
        let m = from_subtitles(&cues, 180.0);
        assert_eq!(m.len(), 3, "{m:?}");
        assert_eq!(m[0].t, 0.0);
        assert_eq!(m[0].name, "大家好，今天介紹 設定 畫面");
        assert_eq!(m[1].name, "第 13 句");
        assert_eq!(m[2].name, "第 26 句");
        assert!(m[1].t > cues[12].end && m[1].t < cues[13].start, "在停頓裡：{}", m[1].t);
        assert!(m.iter().skip(1).all(|x| x.name.starts_with("第 ")));
        // 太短、沒字幕：不產生
        assert!(from_subtitles(&cues, 50.0).is_empty());
        assert!(from_subtitles(&[], 600.0).is_empty());
    }
}
