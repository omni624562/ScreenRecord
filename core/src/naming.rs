//! 自動命名：依錄影裡說的話（字幕）、畫面上的字、錄影時在用的程式與視窗標題，想一個好認的名稱（例如「報價單討論_10-15」）。
//! 只改預設的名稱（Rec_2026-10-15_14-30-05 這種），使用者自己取的名稱不動。

use crate::facets::{candidates, is_cjk};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

/// 想名稱用的線索
#[derive(Debug, Clone, Default)]
pub struct Hints {
    /// 字幕（一句一筆，依時間）
    pub subs: Vec<String>,
    /// 畫面上辨識到的字（一個畫面一筆）
    pub screen: Vec<String>,
    /// 錄影 / 截圖時在用的程式（執行檔名稱，例如 excel.exe）
    pub app: String,
    /// 那個程式的視窗標題
    pub title: String,
    /// 日期（MM-DD），加在名稱後面
    pub date: String,
}

static AUTO_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(Rec|錄音|Replay|Shot)_\d{4}-\d{2}-\d{2}_\d{2}-\d{2}-\d{2}(_\d+)?$").unwrap());

/// 是程式自動取的預設名稱（不含副檔名）：可以自動改名；使用者取過的名稱不動
pub fn is_auto_name(stem: &str) -> bool {
    AUTO_RE.is_match(stem)
}

/// 預設名稱裡的日期（MM-DD）
pub fn date_of(stem: &str) -> Option<String> {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"_\d{4}-(\d{2})-(\d{2})_").unwrap());
    RE.captures(stem).map(|m| format!("{}-{}", &m[1], &m[2]))
}

/// 開頭是這些詞時，後面接的就是這段影片在講什麼
const TOPIC_MARKS: [&str; 14] = ["今天要跟大家", "今天來跟大家", "今天要", "今天來", "這次要", "跟大家示範", "示範", "介紹", "講解", "說明", "分享", "討論", "教大家", "來看"];

/// 主題前面可以拿掉的字
const TOPIC_LEAD: [&str; 9] = ["一下", "跟大家", "給大家", "大家", "的是", "如何", "關於", "：", "的"];

/// 講話常出現、當名稱沒意義的詞
const FILLER: [&str; 30] = [
    "大家",
    "今天",
    "這個",
    "那個",
    "就是",
    "然後",
    "可以",
    "我們",
    "一下",
    "這樣",
    "因為",
    "所以",
    "如果",
    "還有",
    "什麼",
    "現在",
    "怎麼",
    "時候",
    "沒有",
    "知道",
    "應該",
    "其實",
    "感覺",
    "這邊",
    "那邊",
    "的話",
    "謝謝",
    "好的",
    "對對",
    "大家好",
];

/// 標題沒意義、不拿來當名稱
const GENERIC_TITLES: [&str; 10] = ["新增索引標籤", "新分頁", "new tab", "untitled", "未命名", "無標題", "程式管理員", "桌面", "desktop", "home"];

fn clean(s: &str, max: usize) -> String {
    let s: String = s.chars().filter(|c| !r#"\/:*?"<>|"#.contains(*c) && !c.is_control()).collect();
    let s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let s: String = s.chars().take(max).collect();
    s.trim_matches(|c: char| c == '.' || c == ' ' || c == '_' || c == '-').to_string()
}

fn is_break(c: char) -> bool {
    "，。！？、；,.!?;…～~「」『』（）()".contains(c) || c.is_whitespace()
}

/// 開頭幾句裡「今天要介紹…」後面接的主題
fn topic(subs: &[String]) -> Option<String> {
    for s in subs.iter().take(6) {
        for m in TOPIC_MARKS {
            let Some(i) = s.find(m) else { continue };
            let mut rest = &s[i + m.len()..];
            loop {
                let before = rest;
                for lead in TOPIC_LEAD.iter().chain(&TOPIC_MARKS) {
                    rest = rest.trim_start_matches(lead);
                }
                if rest == before {
                    break;
                }
            }
            let t: String = rest.chars().take_while(|c| !is_break(*c)).collect();
            let n = t.chars().count();
            if (2..=16).contains(&n) && t.chars().any(is_cjk) {
                return Some(t);
            }
        }
    }
    None
}

/// 字幕和畫面裡最常出現的詞（至少出現在 2 句 / 2 個畫面）
fn keyword(h: &Hints) -> Option<String> {
    let mut df: HashMap<String, usize> = HashMap::new();
    for s in h.subs.iter().chain(&h.screen) {
        for t in candidates(s) {
            *df.entry(t).or_default() += 1;
        }
    }
    df.retain(|t, d| *d >= 2 && !FILLER.contains(&t.as_str()) && !FILLER.iter().any(|f| t.starts_with(f) || t.ends_with(f)));
    // 被更長、次數差不多的詞包住的片段拿掉（「報價」與「報價單」只留「報價單」）
    let mut drop: HashSet<String> = HashSet::new();
    for (t, &d) in &df {
        let cs: Vec<char> = t.chars().collect();
        for len in 2..cs.len() {
            for w in cs.windows(len) {
                let sub: String = w.iter().collect();
                if df.get(&sub).is_some_and(|&sd| d * 10 >= sd * 7) {
                    drop.insert(sub);
                }
            }
        }
    }
    df.into_iter()
        .filter(|(t, _)| !drop.contains(t))
        .map(|(t, d)| {
            let len = t.chars().count();
            let bonus = if t.chars().next().is_some_and(is_cjk) { 1.0 + 0.3 * (len.saturating_sub(2)) as f64 } else { 0.8 + 0.05 * len.min(10) as f64 };
            (d as f64 * bonus, t)
        })
        .max_by(|a, b| a.0.total_cmp(&b.0).then_with(|| b.1.cmp(&a.1)))
        .map(|(_, t)| t)
}

/// 視窗標題去掉程式名稱（「訂單查詢 - Google Chrome」→「訂單查詢」）；沒意義的標題回傳 None
fn title_part(title: &str, app: &str) -> Option<String> {
    let first = [" - ", " – ", " — ", " | "].iter().fold(title, |t, sep| t.split(sep).next().unwrap_or_default());
    let mut first = first.trim().trim_start_matches(['●', '*', '•']).trim().to_string();
    // 「- Google Chrome」被 split 拆不開的情況（例如只有一段）
    if !app.is_empty() && first.eq_ignore_ascii_case(app) {
        first.clear();
    }
    let t = clean(&first, 24);
    let lower = t.to_lowercase();
    (t.chars().count() >= 2 && !GENERIC_TITLES.contains(&lower.as_str()) && t.chars().any(|c| c.is_alphabetic())).then_some(t)
}

/// 想一個名稱（不含副檔名）；什麼線索都沒有時回傳 None
pub fn suggest(h: &Hints) -> Option<String> {
    let app = if h.app.is_empty() { String::new() } else { crate::shot_meta::app_label(&h.app) };
    let main = topic(&h.subs).or_else(|| keyword(h)).or_else(|| title_part(&h.title, &app)).or_else(|| (!app.is_empty()).then(|| app.clone()))?;
    let main = clean(&main, 30);
    if main.is_empty() {
        return None;
    }
    let name = if h.date.is_empty() { main } else { format!("{main}_{}", h.date) };
    crate::format::check_recording_name(&name).is_none().then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn auto_names_are_recognized() {
        assert!(is_auto_name("Rec_2026-10-15_14-30-05"));
        assert!(is_auto_name("Rec_2026-10-15_14-30-05_2"));
        assert!(is_auto_name("錄音_2026-10-15_14-30-05"));
        assert!(is_auto_name("Shot_2026-10-15_14-30-05"));
        assert!(!is_auto_name("報價單討論"));
        assert!(!is_auto_name("Rec_2026-10-15_14-30-05_cut"));
        assert_eq!(date_of("Rec_2026-10-15_14-30-05").as_deref(), Some("10-15"));
    }

    #[test]
    fn topic_from_the_opening_sentence() {
        let h = Hints { subs: subs(&["大家好", "今天要跟大家介紹報價單的填法，", "首先打開 Excel"]), date: "10-15".into(), ..Default::default() };
        assert_eq!(suggest(&h).as_deref(), Some("報價單的填法_10-15"));
    }

    #[test]
    fn repeated_words_when_there_is_no_topic() {
        let h = Hints {
            subs: subs(&["這份報價單先看總價", "報價單的日期要改", "然後報價單存檔"]), screen: subs(&["報價單 客戶 總價"]), date: "10-15".into(), ..Default::default()
        };
        assert_eq!(suggest(&h).as_deref(), Some("報價單_10-15"));
    }

    #[test]
    fn window_title_then_app_name() {
        let h = Hints { app: "chrome.exe".into(), title: "訂單查詢 - Google Chrome".into(), date: "10-15".into(), ..Default::default() };
        assert_eq!(suggest(&h).as_deref(), Some("訂單查詢_10-15"));
        let h = Hints { app: "excel.exe".into(), title: "新增索引標籤".into(), ..Default::default() };
        assert_eq!(suggest(&h).as_deref(), Some("Excel"));
        assert_eq!(suggest(&Hints::default()), None);
    }

    #[test]
    fn names_are_safe_file_names() {
        let h = Hints { title: "a/b:c*d? - App".into(), ..Default::default() };
        assert_eq!(suggest(&h).as_deref(), Some("abcd"));
    }
}
