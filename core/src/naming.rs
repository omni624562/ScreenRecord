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

// ───────────── 命名規則 ─────────────

/// 命名規則可以用的欄位：（中文, 英文, 說明）
pub const RULE_TOKENS: [(&str, &str); 6] = [("{日期}", "{date}"), ("{時間}", "{time}"), ("{序號}", "{n}"), ("{程式}", "{app}"), ("{視窗}", "{title}"), ("{內容}", "{content}")];

/// 套用命名規則需要的資料
#[derive(Debug, Clone, Default)]
pub struct RuleCtx {
    /// 2026-10-16
    pub date: String,
    /// 14-30-05
    pub time: String,
    /// 錄影 / 截圖時在用的程式（執行檔名稱）
    pub app: String,
    /// 那個程式的視窗標題
    pub title: String,
    /// 依內容想的名稱（{內容}；沒有時空白）
    pub content: String,
}

impl RuleCtx {
    /// 從預設名稱（Rec_2026-10-16_14-30-05）取出日期與時間；不是預設名稱時用現在的時間
    pub fn from_stem(stem: &str) -> RuleCtx {
        static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"_(\d{4}-\d{2}-\d{2})_(\d{2}-\d{2}-\d{2})").unwrap());
        match RE.captures(stem) {
            Some(m) => RuleCtx { date: m[1].to_string(), time: m[2].to_string(), ..Default::default() },
            None => {
                let now = chrono::Local::now();
                RuleCtx { date: now.format("%Y-%m-%d").to_string(), time: now.format("%H-%M-%S").to_string(), ..Default::default() }
            }
        }
    }
}

/// 規則裡有沒有這個欄位（中文或英文的寫法都算）
pub fn rule_uses(rule: &str, token: usize) -> bool {
    let (zh, en) = RULE_TOKENS[token];
    rule.contains(zh) || rule.to_lowercase().contains(en)
}

/// 換掉序號以外的欄位
fn fill(rule: &str, c: &RuleCtx) -> String {
    let app = if c.app.is_empty() { String::new() } else { crate::shot_meta::app_label(&c.app.to_lowercase()) };
    let title: String = c.title.chars().take(40).collect();
    let content: String = c.content.chars().take(40).collect();
    let mut out = rule.to_string();
    for (i, v) in [c.date.as_str(), c.time.as_str(), "\u{E001}", app.as_str(), title.as_str(), content.as_str()].iter().enumerate() {
        let (zh, en) = RULE_TOKENS[i];
        out = out.replace(zh, v);
        // 英文欄位不分大小寫
        let re = Regex::new(&format!("(?i){}", regex::escape(en))).unwrap();
        out = re.replace_all(&out, regex::NoExpand(v)).into_owned();
    }
    out
}

/// 去掉檔名不能用的字、空掉的欄位留下的多餘分隔符號
fn tidy_name(s: &str) -> String {
    static BAD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"[\\/:*?"<>|\x00-\x1f]"#).unwrap());
    // 空掉的欄位兩邊的分隔符號：「_ _」「__」只留一個；「 - 」這種刻意的寫法不動
    static SEP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"([_\-.]) *[_\-.]+").unwrap());
    static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r" {2,}").unwrap());
    let s = BAD.replace_all(s, " ");
    let s = SEP.replace_all(&s, "$1");
    let s = SPACES.replace_all(&s, " ");
    let s: String = s.trim_matches(|c: char| c == ' ' || c == '_' || c == '-' || c == '.').chars().take(100).collect();
    s.trim_end_matches(['.', ' ']).to_string()
}

/// 套用命名規則（不含副檔名）。{序號} 是同一個資料夾裡、其他部分都一樣的檔案的下一個號碼（001、002…）；
/// exists：這個檔名（含副檔名）已經有了。結果是空的、或不是合法的檔名時 None
pub fn apply_rule(rule: &str, c: &RuleCtx, ext: &str, existing: &[String]) -> Option<String> {
    let rule = rule.trim();
    if rule.is_empty() {
        return None;
    }
    let filled = fill(rule, c);
    let name = if filled.contains('\u{E001}') {
        // 已經用過的號碼：其他部分一樣、序號是數字的檔案
        let pat = tidy_name(&filled.replace('\u{E001}', "\u{E002}"));
        let (head, tail) = pat.split_once('\u{E002}').unwrap_or((&pat, ""));
        let re = Regex::new(&format!(r"(?i)^{}(\d+){}(?:_\d+)?\.{}$", regex::escape(head), regex::escape(tail), regex::escape(ext))).ok()?;
        let max = existing.iter().filter_map(|f| re.captures(f).and_then(|m| m[1].parse::<u32>().ok())).max().unwrap_or(0);
        format!("{head}{:03}{tail}", max + 1)
    } else {
        tidy_name(&filled)
    };
    let name = tidy_name(&name);
    (!name.is_empty() && crate::format::check_recording_name(&name).is_none()).then_some(name)
}

/// 設定畫面的範例：用現在的時間、「Excel」試套一次
pub fn rule_preview(rule: &str, ext: &str) -> Option<String> {
    let now = chrono::Local::now();
    let c = RuleCtx {
        date: now.format("%Y-%m-%d").to_string(),
        time: now.format("%H-%M-%S").to_string(),
        app: "excel.exe".into(),
        title: crate::tr!("報價單.xlsx", "Quote.xlsx").into(),
        content: crate::tr!("報價單討論", "Quote review").into(),
    };
    apply_rule(rule, &c, ext, &[]).map(|n| format!("{n}.{ext}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn naming_rules() {
        let c = RuleCtx { date: "2026-10-16".into(), time: "14-30-05".into(), app: "EXCEL.EXE".into(), title: "報價單: 第2版".into(), content: String::new() };
        assert_eq!(apply_rule("客戶A_{日期}_{序號}", &c, "mp4", &[]).as_deref(), Some("客戶A_2026-10-16_001"));
        let have = vec!["客戶A_2026-10-16_001.mp4".to_string(), "客戶A_2026-10-16_007_2.MP4".into(), "客戶A_2026-10-15_009.mp4".into(), "客戶A_2026-10-16_x.mp4".into()];
        assert_eq!(apply_rule("客戶A_{日期}_{序號}", &c, "mp4", &have).as_deref(), Some("客戶A_2026-10-16_008"));
        // 不同副檔名分開算
        assert_eq!(apply_rule("客戶A_{日期}_{序號}", &c, "png", &have).as_deref(), Some("客戶A_2026-10-16_001"));
        // 英文欄位、不能用的字換掉
        assert_eq!(apply_rule("{APP} - {title} {time}", &c, "mp4", &[]).as_deref(), Some("Excel - 報價單 第2版 14-30-05"));
        // 空掉的欄位不留多餘的分隔
        assert_eq!(apply_rule("{內容}_{日期}", &c, "mp4", &[]).as_deref(), Some("2026-10-16"));
        assert_eq!(apply_rule("{內容}", &c, "mp4", &[]), None);
        assert_eq!(apply_rule("  ", &c, "mp4", &[]), None);
        assert!(rule_uses("a{序號}", 2) && rule_uses("a{N}", 2) && !rule_uses("{日期}", 2));
        let r = RuleCtx::from_stem("Rec_2026-10-16_14-30-05_2");
        assert_eq!((r.date.as_str(), r.time.as_str()), ("2026-10-16", "14-30-05"));
    }

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
