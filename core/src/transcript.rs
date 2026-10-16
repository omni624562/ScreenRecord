//! 逐字稿：把字幕（剪輯後的時間）整理成一段一段的文字，存成 TXT 或 Word。
//! 句子之間停了比較久就換一段；可以加上每段開始的時間、去掉只有「嗯、呃」的句子與句首的語助詞

use crate::subtitles::Cue;

/// 句子之間沒講話超過幾秒就換一段
pub const PARAGRAPH_GAP: f64 = 2.0;
/// 一段最多幾句（太長不好讀）
const MAX_SENTENCES: usize = 8;

/// 一段：開始的時間（秒）與文字
#[derive(Debug, Clone, PartialEq)]
pub struct Paragraph {
    pub start: f64,
    pub text: String,
}

/// 句首的語助詞（後面接標點或空白才去掉，不會吃掉「那個人」的「那個」）
const LEADING: [&str; 8] = ["嗯", "呃", "欸", "喔", "哦", "um", "uh", "er"];

fn strip_leading(text: &str) -> String {
    let mut t = text.trim();
    loop {
        let before = t;
        for w in LEADING {
            if let Some(rest) = t.strip_prefix(w) {
                let rest_trim = rest.trim_start_matches(['，', '、', ',', '…', '.', '。', ' ']);
                if rest_trim.len() < rest.len() || rest.is_empty() {
                    t = rest_trim.trim_start();
                }
            }
        }
        if t == before {
            return t.to_string();
        }
    }
}

fn is_punct(c: char) -> bool {
    "，。！？、；：…,.!?;:」』）)".contains(c)
}

fn is_cjk(c: char) -> bool {
    ('\u{3400}'..='\u{9fff}').contains(&c) || ('\u{f900}'..='\u{faff}').contains(&c)
}

/// 接在前一句後面：辨識出來的中文常常沒有標點，補上逗號；英文中間加空白
fn join(a: &str, b: &str) -> String {
    let Some(last) = a.chars().last() else { return b.to_string() };
    let first = b.chars().next().unwrap_or(' ');
    if is_punct(last) {
        if last.is_ascii() || first.is_ascii() {
            format!("{a} {b}")
        } else {
            format!("{a}{b}")
        }
    } else if is_cjk(last) || is_cjk(first) {
        format!("{a}，{b}")
    } else {
        format!("{a} {b}")
    }
}

/// 一段的結尾補上句號（中文）
fn finish(text: &mut String) {
    if text.chars().last().is_some_and(|c| is_cjk(c) && !is_punct(c)) {
        text.push('。');
    }
}

/// 整理成段落；clean = 去掉贅字句與句首語助詞
pub fn paragraphs(cues: &[Cue], clean: bool) -> Vec<Paragraph> {
    let mut out: Vec<Paragraph> = vec![];
    let mut last_end = f64::NEG_INFINITY;
    let mut count = 0;
    for c in cues {
        let text = c.text.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ");
        let text = if clean { strip_leading(&text) } else { text };
        if text.is_empty() || (clean && crate::fillers::is_filler(&c.text)) {
            continue;
        }
        let new_para = out.is_empty() || c.start - last_end > PARAGRAPH_GAP || count >= MAX_SENTENCES;
        if new_para {
            if let Some(p) = out.last_mut() {
                finish(&mut p.text);
            }
            out.push(Paragraph { start: c.start, text });
            count = 1;
        } else if let Some(p) = out.last_mut() {
            p.text = join(&p.text, &text);
            count += 1;
        }
        last_end = c.end;
    }
    if let Some(p) = out.last_mut() {
        finish(&mut p.text);
    }
    out
}

/// 時間標記 [MM:SS]（超過 1 小時 [H:MM:SS]）
pub fn stamp(sec: f64) -> String {
    let s = sec.max(0.0).floor() as u64;
    if s >= 3600 {
        format!("[{}:{:02}:{:02}]", s / 3600, (s % 3600) / 60, s % 60)
    } else {
        format!("[{:02}:{:02}]", s / 60, s % 60)
    }
}

/// 純文字逐字稿（Windows 換行）：標題、空一行，每段之間空一行
pub fn text(title: &str, paras: &[Paragraph], with_time: bool) -> String {
    let mut s = format!("{title}\r\n\r\n");
    for p in paras {
        if with_time {
            s.push_str(&stamp(p.start));
            s.push(' ');
        }
        s.push_str(&p.text);
        s.push_str("\r\n\r\n");
    }
    s
}

/// Word 逐字稿
pub fn docx(title: &str, subtitle: &str, paras: &[Paragraph], with_time: bool) -> Vec<u8> {
    let list: Vec<(Option<String>, String)> = paras.iter().map(|p| (with_time.then(|| stamp(p.start)), p.text.clone())).collect();
    crate::ooxml::docx_text(title, subtitle, &list)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(s: f64, e: f64, t: &str) -> Cue {
        Cue { start: s, end: e, text: t.into() }
    }

    #[test]
    fn groups_into_paragraphs() {
        let cues = [cue(0.0, 2.0, "大家好"), cue(2.2, 3.0, "嗯"), cue(3.1, 5.0, "嗯，今天介紹報表"), cue(9.0, 10.0, "接著看第二頁"), cue(10.2, 11.0, "Click Save.")];
        let p = paragraphs(&cues, true);
        assert_eq!(p, vec![Paragraph { start: 0.0, text: "大家好，今天介紹報表。".into() }, Paragraph { start: 9.0, text: "接著看第二頁，Click Save.".into() }]);
        let raw = paragraphs(&cues, false);
        assert_eq!(raw[0].text, "大家好，嗯，嗯，今天介紹報表。");
        assert_eq!(text("會議", &p, true), "會議\r\n\r\n[00:00] 大家好，今天介紹報表。\r\n\r\n[00:09] 接著看第二頁，Click Save.\r\n\r\n");
        // 已經有標點的不再加
        assert_eq!(paragraphs(&[cue(0.0, 1.0, "好的。"), cue(1.1, 2.0, "下一步？")], false)[0].text, "好的。下一步？");
        assert_eq!(stamp(3725.0), "[1:02:05]");
        // 英文句子之間加空白；「那個人」不會被當成語助詞
        let en = paragraphs(&[cue(0.0, 1.0, "Hello there."), cue(1.1, 2.0, "uh, next step")], true);
        assert_eq!(en[0].text, "Hello there. next step");
        assert_eq!(strip_leading("那個人很好"), "那個人很好");
        assert_eq!(strip_leading("呃 好"), "好");
    }

    #[test]
    fn word_document_is_a_zip() {
        let d = docx("逐字稿", "2026-10-16", &[Paragraph { start: 1.0, text: "A & B <c>".into() }], true);
        assert_eq!(&d[..2], b"PK");
        let s = String::from_utf8_lossy(&d);
        assert!(s.contains("A &amp; B &lt;c&gt;") && s.contains("[00:01]"));
    }
}
