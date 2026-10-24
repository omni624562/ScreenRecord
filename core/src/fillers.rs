//! 贅字與停頓：從字幕找出只有「嗯、呃、那個」的句子，以及句子之間太久沒講話的地方，讓剪輯視窗一次剪掉。
//! 字幕只有一句一句的時間（沒有每個字的時間），所以句子裡夾著的贅字剪不掉，只找整句都是贅字的。

use crate::edit::Range;
use crate::subtitles::Cue;

/// 找到的一段
#[derive(Debug, Clone, PartialEq)]
pub struct Filler {
    pub start: f64,
    pub end: f64,
    pub kind: Kind,
    /// 贅字的那句話（停頓時空白）
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 整句都是贅字
    Word,
    /// 句子之間沒講話
    Pause,
}

impl Filler {
    pub fn len(&self) -> f64 {
        self.end - self.start
    }

    /// 預設要不要剪：贅字剪；停頓超過 LONG_PAUSE 秒時可能是在操作畫面（示範），預設不剪
    pub fn default_on(&self) -> bool {
        self.kind == Kind::Word || self.len() <= LONG_PAUSE
    }
}

/// 只有這些字（可以重複，例如「嗯嗯」「那個那個」）的句子算贅字；長的放前面
const WORDS: [&str; 22] = ["you know", "那個", "這個", "就是", "然後", "所以", "hmm", "嗯", "呃", "啊", "欸", "喔", "哦", "唉", "恩", "额", "um", "uh", "er", "ah", "eh", "mm"];

/// 句子之間沒講話超過幾秒才算停頓
pub const MIN_PAUSE: f64 = 1.0;
/// 停頓前後各留幾秒，剪完聽起來才自然（不會一句接一句黏在一起）
pub const PAD: f64 = 0.25;
/// 超過幾秒的停頓預設不剪（可能在操作畫面）
pub const LONG_PAUSE: f64 = 4.0;

/// 這句話只有贅字
pub fn is_filler(text: &str) -> bool {
    let mut t: String = text.to_lowercase().chars().filter(|c| c.is_alphanumeric() || *c == ' ').collect::<String>();
    t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.is_empty() {
        return false;
    }
    loop {
        let before = t.len();
        for w in WORDS {
            t = t.trim_start().to_string();
            if let Some(rest) = t.strip_prefix(w) {
                // 英文的贅字後面要是空白或結尾（「um」不能吃掉「umbrella」）
                if w.is_ascii() && rest.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()) {
                    continue;
                }
                t = rest.to_string();
            }
        }
        if t.trim().is_empty() {
            return true;
        }
        if t.len() == before {
            return false;
        }
    }
}

/// 找出贅字的句子與句子之間的停頓（已經剪掉的不算）；依時間排序
pub fn find(cues: &[Cue], removed: &[Range], duration: f64) -> Vec<Filler> {
    let inside_removed = |a: f64, b: f64| removed.iter().any(|(x, y)| *x <= a + 0.01 && *y >= b - 0.01);
    let mut cues: Vec<&Cue> = cues.iter().collect();
    cues.sort_by(|a, b| a.start.total_cmp(&b.start));
    let mut out = vec![];
    // 停頓：贅字的句子也當作沒講話，和前後的停頓接在一起
    let speech: Vec<&&Cue> = cues.iter().filter(|c| !is_filler(&c.text)).collect();
    for c in &cues {
        if is_filler(&c.text) && !inside_removed(c.start, c.end) {
            out.push(Filler { start: c.start, end: c.end, kind: Kind::Word, text: c.text.trim().to_string() });
        }
    }
    for w in speech.windows(2) {
        let (a, b) = (w[0].end + PAD, w[1].start - PAD);
        if w[1].start - w[0].end >= MIN_PAUSE && b > a && !inside_removed(a, b) {
            out.push(Filler { start: a, end: b, kind: Kind::Pause, text: String::new() });
        }
    }
    // 贅字的句子大半已經在停頓裡（停頓前後留了一點）就不另外列
    let pauses: Vec<(f64, f64)> = out.iter().filter(|f| f.kind == Kind::Pause).map(|f| (f.start, f.end)).collect();
    out.retain(|f| f.kind == Kind::Pause || !pauses.iter().any(|(a, b)| (b.min(f.end) - a.max(f.start)) * 2.0 > f.len()));
    out.retain(|f| f.end <= duration + 0.01);
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(a: f64, b: f64, t: &str) -> Cue {
        Cue { start: a, end: b, text: t.into() }
    }

    #[test]
    fn filler_sentences() {
        for t in ["嗯", "呃…", "那個那個，", "嗯嗯 呃", "Um, uh...", "you know"] {
            assert!(is_filler(t), "{t}");
        }
        for t in ["那個檔案", "嗯，好的", "umbrella", "就是這裡", ""] {
            assert!(!is_filler(t), "{t}");
        }
    }

    #[test]
    fn finds_fillers_and_pauses_but_not_removed_parts() {
        let cues = [c(0.0, 2.0, "大家好"), c(2.1, 3.0, "嗯"), c(3.1, 5.0, "今天介紹報價單"), c(7.0, 9.0, "先開啟檔案"), c(20.0, 22.0, "完成了")];
        let f = find(&cues, &[], 30.0);
        // 2.0～3.1 只有「嗯」：當作停頓（贅字併在裡面）
        let got: Vec<(f64, f64, Kind)> = f.iter().map(|x| ((x.start * 100.0).round() / 100.0, (x.end * 100.0).round() / 100.0, x.kind)).collect();
        assert_eq!(got, vec![(2.25, 2.85, Kind::Pause), (5.25, 6.75, Kind::Pause), (9.25, 19.75, Kind::Pause)]);
        assert!(f[0].default_on() && f[1].default_on() && !f[2].default_on());
        // 已經剪掉的停頓不列
        let f = find(&cues, &[(5.0, 7.0)], 30.0);
        assert!(!f.iter().any(|x| x.start > 5.0 && x.end < 7.0));
    }

    #[test]
    fn short_gap_filler_is_listed_on_its_own() {
        let cues = [c(0.0, 2.0, "大家好"), c(2.05, 2.5, "呃"), c(2.55, 4.0, "開始吧")];
        let f = find(&cues, &[], 10.0);
        assert_eq!(f.len(), 1);
        assert_eq!((f[0].kind, f[0].text.as_str()), (Kind::Word, "呃"));
    }
}
