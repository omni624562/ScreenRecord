//! 截圖的瀏覽分類：不用記得關鍵字，用點的找圖。
//!
//! 每張截圖從這些資料分類：
//! - 截圖當下的程式（shot_meta）
//! - 圖裡的字（ocr_index）：內容類型（錯誤訊息、網頁、對話…）與常見詞
//! - 主要顏色（similar 的指紋快取裡一起算）
//! - 日期、時段，以及長截圖、編輯過、有星號等
//!
//! 瀏覽欄的每個選項都附上「加上這個條件後會剩幾張」。

use crate::ocr_index::OcrIndex;
use crate::pins::PinStore;
use crate::shot_meta::{ShotMeta, ShotMetaStore};
use crate::similar::HashStore;
use crate::tr;
use crate::types::{FacetCounts, FacetOption, FacetQuery};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

// ───────────── 內容類型 ─────────────

pub const KIND_ERROR: u16 = 1;
pub const KIND_WEB: u16 = 2;
pub const KIND_CHAT: u16 = 4;
pub const KIND_TABLE: u16 = 8;
pub const KIND_CODE: u16 = 16;
pub const KIND_MONEY: u16 = 32;
pub const KIND_DOC: u16 = 64;
pub const KIND_NOTEXT: u16 = 128;
pub const KIND_PERSONAL: u16 = 256;

const KINDS: [(u16, &str); 9] = [
    (KIND_ERROR, "error"),
    (KIND_WEB, "web"),
    (KIND_CHAT, "chat"),
    (KIND_TABLE, "table"),
    (KIND_CODE, "code"),
    (KIND_MONEY, "money"),
    (KIND_DOC, "doc"),
    (KIND_NOTEXT, "notext"),
    (KIND_PERSONAL, "personal"),
];

fn kind_label(id: &str) -> String {
    match id {
        "error" => tr!("錯誤訊息", "Error messages"),
        "web" => tr!("網頁", "Web pages"),
        "chat" => tr!("對話", "Chats"),
        "table" => tr!("表格、數字", "Tables & numbers"),
        "code" => tr!("程式碼", "Code"),
        "money" => tr!("金額、訂單", "Prices & orders"),
        "doc" => tr!("文件", "Documents"),
        "notext" => tr!("幾乎沒字", "Little or no text"),
        "personal" => tr!("含個資", "Personal info"),
        _ => "",
    }
    .to_string()
}

fn contains_any(text: &str, words: &[&str]) -> bool {
    words.iter().any(|w| text.contains(w))
}

/// 從圖裡的字判斷內容類型（KIND_* 位元）。text：正規化過的字（小寫、沒有空白）
pub fn text_kinds(text: &str) -> u16 {
    static TIME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(上午|下午|晚上|早上)\d{1,2}:\d{2}|\d{1,2}:\d{2}(am|pm)").unwrap());
    static EMAIL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-z0-9._%+-]+@[a-z0-9.-]+\.[a-z]{2,}").unwrap());
    static MONEY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(nt\$|us\$|\$|¥|€)\d|\d元").unwrap());
    static PHONE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(^|\D)09\d{2}-?\d{3}-?\d{3}(\D|$)|(^|[^a-z])[a-z][12]\d{8}(\D|$)").unwrap());
    let chars = text.chars().count();
    if chars < 8 {
        return KIND_NOTEXT;
    }
    let mut k = 0;
    if contains_any(
        text,
        &["error", "錯誤", "失敗", "exception", "failed", "failure", "無法", "denied", "拒絕", "找不到", "notfound", "警告", "warning", "crash", "timeout", "逾時", "發生問題", "fatal", "traceback"],
    ) {
        k |= KIND_ERROR;
    }
    if contains_any(text, &["http://", "https://", "www.", ".com", ".com.tw", ".gov.tw", ".org"]) {
        k |= KIND_WEB;
    }
    if contains_any(text, &["已讀", "已傳送", "輸入訊息", "typeamessage", "回覆", "已收回"]) || TIME.find_iter(text).count() >= 2 {
        k |= KIND_CHAT;
    }
    let digits = text.chars().filter(|c| c.is_ascii_digit()).count();
    if chars >= 40 && digits * 10 >= chars * 3 {
        k |= KIND_TABLE;
    }
    let sym = text.chars().filter(|c| "{}();=<>[]".contains(*c)).count();
    if (chars >= 30 && sym * 100 >= chars * 6) || contains_any(text, &["function(", "return", "import", "#include", "publicstatic", "console.log", "def", "const", "select*from"]) && sym >= 4 {
        k |= KIND_CODE;
    }
    if MONEY.is_match(text) || contains_any(text, &["金額", "總計", "合計", "小計", "訂單", "發票", "付款", "報價", "invoice", "subtotal", "order#"]) {
        k |= KIND_MONEY;
    }
    if chars >= 400 && k & (KIND_CODE | KIND_TABLE) == 0 {
        k |= KIND_DOC;
    }
    if EMAIL.is_match(text) || PHONE.is_match(text) {
        k |= KIND_PERSONAL;
    }
    k
}

/// 從截圖當下的程式補上內容類型（瀏覽器 → 網頁、LINE → 對話…）
pub fn app_kinds(app: &str) -> u16 {
    let stem = app.trim_end_matches(".exe");
    match stem {
        "chrome" | "msedge" | "firefox" | "brave" | "opera" | "vivaldi" => KIND_WEB,
        "line" | "ms-teams" | "teams" | "slack" | "discord" | "telegram" | "whatsapp" | "messenger" => KIND_CHAT,
        "excel" => KIND_TABLE,
        "code" | "devenv" | "idea64" | "pycharm64" | "windowsterminal" | "cmd" | "powershell" | "pwsh" => KIND_CODE,
        "winword" | "acrord32" | "acrobat" | "notepad" | "onenote" => KIND_DOC,
        _ => 0,
    }
}

// ───────────── 常見詞 ─────────────

/// 一個常見詞與出現在幾張截圖
#[derive(Debug, Clone, PartialEq)]
pub struct Term {
    pub text: String,
    pub df: usize,
    /// 排序用的分數（出現的張數、有沒有辨識度、長度）
    pub score: f64,
}

/// 一張截圖的分析：有哪些常見詞（索引）、內容類型（只看字）、有沒有字
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DocInfo {
    pub terms: Vec<u32>,
    pub kinds: u16,
    pub has_text: bool,
}

/// 全部截圖的字整理出來的詞彙
#[derive(Debug, Clone, Default)]
pub struct Vocab {
    /// 依分數由高到低
    pub terms: Vec<Term>,
    /// 路徑鍵（OcrIndex::key）→ 這張的分析
    pub docs: HashMap<String, DocInfo>,
    /// 有字的截圖張數
    pub n: usize,
}

pub(crate) fn is_cjk(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c) || ('\u{3400}'..='\u{4dbf}').contains(&c)
}

/// 開頭或結尾是這些字的中文片段不算詞（「的設」「設定了」）
const CJK_STOP: &str = "的了是在和與及或我你他她它們這那有也就都而被把從對為以之其個一不上下中";

const EN_STOP: [&str; 40] = [
    "the", "and", "for", "with", "you", "your", "this", "that", "are", "was", "from", "http", "https", "www", "com", "not", "but", "have", "has", "can", "will", "all", "any", "our", "out", "use",
    "new", "more", "one", "get", "its", "into", "than", "then", "them", "they", "what", "when", "which", "who",
];

/// 一段字裡的候選詞：中文連續 2～4 字的片段、英文 3 個字母以上的單字（不含純數字）
pub(crate) fn candidates(words: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut run: Vec<char> = vec![];
    let mut latin = String::new();
    let flush_cjk = |run: &mut Vec<char>, out: &mut HashSet<String>| {
        for n in 2..=4usize {
            if run.len() < n {
                break;
            }
            for w in run.windows(n) {
                if CJK_STOP.contains(w[0]) || CJK_STOP.contains(w[n - 1]) {
                    continue;
                }
                out.insert(w.iter().collect());
            }
        }
        run.clear();
    };
    let flush_latin = |latin: &mut String, out: &mut HashSet<String>| {
        let n = latin.chars().count();
        if (3..=24).contains(&n) && latin.chars().any(|c| c.is_ascii_alphabetic()) && !EN_STOP.contains(&latin.as_str()) {
            out.insert(latin.clone());
        }
        latin.clear();
    };
    for c in words.chars() {
        if is_cjk(c) {
            flush_latin(&mut latin, &mut out);
            run.push(c);
        } else if c.is_ascii_alphanumeric() {
            flush_cjk(&mut run, &mut out);
            latin.push(c.to_ascii_lowercase());
        } else {
            flush_cjk(&mut run, &mut out);
            flush_latin(&mut latin, &mut out);
        }
    }
    flush_cjk(&mut run, &mut out);
    flush_latin(&mut latin, &mut out);
    out
}

/// 整理詞彙：出現在至少 2 張、但不到一半的詞（太常見的沒有辨識度）；
/// 中文片段被更長、出現次數差不多的詞包住時只留長的（「報價」與「報價單」只留「報價單」）
pub fn build_vocab(docs: &[(String, String)]) -> Vocab {
    let mut sets: Vec<(String, HashSet<String>, u16, bool)> = Vec::with_capacity(docs.len());
    let mut df: HashMap<String, usize> = HashMap::new();
    for (key, words) in docs {
        let norm = crate::ocr_index::normalize(words);
        let has_text = !norm.is_empty();
        let set = candidates(words);
        for t in &set {
            *df.entry(t.clone()).or_default() += 1;
        }
        sets.push((key.clone(), set, text_kinds(&norm), has_text));
    }
    let n = sets.iter().filter(|s| s.3).count();
    let max_df = if n >= 6 { n / 2 } else { n };
    let mut kept: HashMap<String, usize> = df.into_iter().filter(|(_, d)| *d >= 2 && *d <= max_df.max(2)).collect();
    // 被更長的詞包住、次數差不多的中文片段移除
    let mut drop: HashSet<String> = HashSet::new();
    for (t, &d) in &kept {
        let cs: Vec<char> = t.chars().collect();
        if cs.len() < 3 || !is_cjk(cs[0]) {
            continue;
        }
        for len in 2..cs.len() {
            for w in cs.windows(len) {
                let sub: String = w.iter().collect();
                if let Some(&sd) = kept.get(&sub) {
                    if d * 10 >= sd * 7 {
                        drop.insert(sub);
                    }
                }
            }
        }
    }
    kept.retain(|t, _| !drop.contains(t));
    let total = n.max(1) as f64;
    let mut terms: Vec<Term> = kept
        .into_iter()
        .map(|(text, df)| {
            let len = text.chars().count();
            let bonus = if text.chars().next().is_some_and(is_cjk) { 1.0 + 0.25 * (len.saturating_sub(2)) as f64 } else { 1.0 + 0.05 * len.min(10) as f64 };
            let score = df as f64 * (total / df as f64 + 1.0).ln() * bonus;
            Term { text, df, score }
        })
        .collect();
    terms.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.text.cmp(&b.text)));
    let index: HashMap<&str, u32> = terms.iter().enumerate().map(|(i, t)| (t.text.as_str(), i as u32)).collect();
    let docs = sets
        .into_iter()
        .map(|(key, set, kinds, has_text)| {
            let mut ids: Vec<u32> = set.iter().filter_map(|t| index.get(t.as_str()).copied()).collect();
            ids.sort_unstable();
            (key, DocInfo { terms: ids, kinds, has_text })
        })
        .collect();
    Vocab { terms, docs, n }
}

/// 詞彙的快取：文字索引變了才重算（索引還在建立時最多一分鐘算一次）
#[derive(Default)]
pub struct VocabCache {
    inner: Mutex<Option<(u64, Instant, Arc<Vocab>)>>,
}

impl VocabCache {
    pub fn get(&self, ocr: &OcrIndex) -> Arc<Vocab> {
        let generation = ocr.generation();
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((g, at, v)) = inner.as_ref() {
            let busy = ocr.progress().is_some();
            if *g == generation || (busy && at.elapsed().as_secs() < 60) {
                return v.clone();
            }
        }
        let t = Instant::now();
        let v = Arc::new(build_vocab(&ocr.docs()));
        crate::info!("[瀏覽] 整理 {} 張截圖的字：{} 個詞，{} 毫秒", v.n, v.terms.len(), t.elapsed().as_millis());
        *inner = Some((generation, Instant::now(), v.clone()));
        v
    }
}

// ───────────── 中英對照、建議 ─────────────

/// 搜尋時一起找的同義詞（中英對照）；query 已正規化
pub fn synonyms(query: &str) -> Vec<String> {
    const PAIRS: [(&str, &str); 24] = [
        ("錯誤", "error"),
        ("失敗", "fail"),
        ("登入", "login"),
        ("登入", "signin"),
        ("登出", "logout"),
        ("密碼", "password"),
        ("設定", "setting"),
        ("帳號", "account"),
        ("訂單", "order"),
        ("發票", "invoice"),
        ("會議", "meeting"),
        ("報價", "quot"),
        ("付款", "payment"),
        ("搜尋", "search"),
        ("下載", "download"),
        ("上傳", "upload"),
        ("警告", "warning"),
        ("通知", "notification"),
        ("檔案", "file"),
        ("使用者", "user"),
        ("客戶", "customer"),
        ("價格", "price"),
        ("總計", "total"),
        ("找不到", "notfound"),
    ];
    let mut out = vec![];
    for (zh, en) in PAIRS {
        if query == zh || (query.len() >= 3 && en.starts_with(query) && query.is_ascii()) || query == en {
            for w in [zh, en] {
                if w != query && !out.iter().any(|x: &String| x == w) {
                    out.push(w.to_string());
                }
            }
        }
    }
    out
}

/// 打字時的建議：圖裡真的有、包含這幾個字的詞，出現越多張越前面
pub fn suggest(vocab: &Vocab, query: &str, limit: usize) -> Vec<(String, usize)> {
    let q = crate::ocr_index::normalize(query);
    if q.is_empty() {
        return vec![];
    }
    let fq = crate::ocr_index::fold(&q);
    let mut hits: Vec<&Term> = vocab.terms.iter().filter(|t| t.text.contains(&q) || crate::ocr_index::fold(&t.text).contains(&fq)).collect();
    hits.sort_by(|a, b| b.df.cmp(&a.df).then_with(|| a.text.chars().count().cmp(&b.text.chars().count())));
    hits.into_iter().take(limit).map(|t| (t.text.clone(), t.df)).collect()
}

// ───────────── 分類一張截圖 ─────────────

pub const OTHER_LONG: u8 = 1;
pub const OTHER_EDITED: u8 = 2;
pub const OTHER_STARRED: u8 = 4;
pub const OTHER_FRAME: u8 = 8;
pub const OTHER_TEXT: u8 = 16;

const OTHERS: [(u8, &str); 5] = [(OTHER_STARRED, "starred"), (OTHER_TEXT, "text"), (OTHER_LONG, "long"), (OTHER_EDITED, "edited"), (OTHER_FRAME, "frame")];

fn other_label(id: &str) -> String {
    match id {
        "starred" => tr!("有星號", "Starred"),
        "text" => tr!("有字", "Has text"),
        "long" => tr!("長截圖", "Scrolling screenshots"),
        "edited" => tr!("編輯過", "Edited"),
        "frame" => tr!("影片畫面", "Video frames"),
        _ => "",
    }
    .to_string()
}

const COLORS: [(u8, &str); 7] = [
    (crate::similar::COLOR_DARK, "dark"),
    (crate::similar::COLOR_LIGHT, "light"),
    (crate::similar::COLOR_RED, "red"),
    (crate::similar::COLOR_YELLOW, "yellow"),
    (crate::similar::COLOR_GREEN, "green"),
    (crate::similar::COLOR_BLUE, "blue"),
    (crate::similar::COLOR_PURPLE, "purple"),
];

fn color_label(id: &str) -> String {
    match id {
        "dark" => tr!("深色畫面", "Dark"),
        "light" => tr!("淺色畫面", "Light"),
        "red" => tr!("紅色", "Red"),
        "yellow" => tr!("黃色、橘色", "Yellow & orange"),
        "green" => tr!("綠色", "Green"),
        "blue" => tr!("藍色", "Blue"),
        "purple" => tr!("紫色", "Purple"),
        _ => "",
    }
    .to_string()
}

const WHENS: [(u8, &str); 6] = [(1, "today"), (2, "yesterday"), (4, "week"), (8, "month"), (16, "lastmonth"), (32, "older")];

fn when_label(id: &str) -> String {
    match id {
        "today" => tr!("今天", "Today"),
        "yesterday" => tr!("昨天", "Yesterday"),
        "week" => tr!("最近 7 天", "Last 7 days"),
        "month" => tr!("這個月", "This month"),
        "lastmonth" => tr!("上個月", "Last month"),
        "older" => tr!("更早", "Earlier"),
        _ => "",
    }
    .to_string()
}

const HOURS: [(u8, &str); 3] = [(1, "morning"), (2, "afternoon"), (4, "evening")];

fn hour_label(id: &str) -> String {
    match id {
        "morning" => tr!("上午", "Morning"),
        "afternoon" => tr!("下午", "Afternoon"),
        "evening" => tr!("晚上", "Evening"),
        _ => "",
    }
    .to_string()
}

/// 一張截圖的分類
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Facets {
    /// 子資料夾（MediaInfo::folder；分類時不知道，列清單時填上）
    pub folder: String,
    pub app: String,
    pub kinds: u16,
    pub terms: Vec<u32>,
    pub when: u8,
    pub hours: u8,
    pub colors: u8,
    pub other: u8,
    /// 2026-10-08
    pub day: String,
    /// 有沒有讀過字（還沒讀的不算進內容類型）
    pub read: bool,
}

/// 分類要用的資料（每次列清單取一次）
pub struct Sources<'a> {
    pub ocr: &'a OcrIndex,
    pub meta: &'a ShotMetaStore,
    pub hashes: &'a HashStore,
    pub pins: &'a PinStore,
    pub vocab: &'a VocabCache,
}

/// 一次取好的資料
pub struct Prepared {
    pub vocab: Arc<Vocab>,
    meta: HashMap<String, ShotMeta>,
    colors: HashMap<String, (crate::similar::Print, u8)>,
    pins: HashSet<String>,
    now: chrono::NaiveDateTime,
}

pub fn prepare(src: &Sources) -> Prepared {
    Prepared {
        vocab: src.vocab.get(src.ocr),
        meta: src.meta.all(),
        colors: src.hashes.snapshot(),
        pins: src.pins.list().into_iter().map(|p| p.to_lowercase()).collect(),
        now: chrono::Local::now().naive_local(),
    }
}

impl Prepared {
    pub fn meta(&self, path: &str) -> Option<&ShotMeta> {
        self.meta.get(&ShotMetaStore::key(path))
    }

    pub fn print(&self, path: &str) -> Option<crate::similar::Print> {
        self.colors.get(&HashStore::key(path)).map(|(p, _)| *p)
    }
}

/// 日期範圍與時段的位元
fn time_bits(t: chrono::NaiveDateTime, now: chrono::NaiveDateTime) -> (u8, u8) {
    use chrono::{Datelike, Timelike};
    let (d, today) = (t.date(), now.date());
    let days = (today - d).num_days();
    let mut w = 0u8;
    if days == 0 {
        w |= 1;
    }
    if days == 1 {
        w |= 2;
    }
    if (0..7).contains(&days) {
        w |= 4;
    }
    let this_month = (today.year(), today.month());
    let last_month = if today.month() == 1 { (today.year() - 1, 12) } else { (today.year(), today.month() - 1) };
    let m = (d.year(), d.month());
    if m == this_month {
        w |= 8;
    } else if m == last_month {
        w |= 16;
    } else if m < last_month {
        w |= 32;
    }
    let h = match t.hour() {
        5..=11 => 1,
        12..=17 => 2,
        _ => 4,
    };
    (w, h)
}

/// 分類一張截圖。time_ms：截圖的時間（檔名或修改時間）；w、h：寬高（不知道時 0）
pub fn describe(prep: &Prepared, path: &str, name: &str, time_ms: f64, w: u32, h: u32) -> Facets {
    use chrono::TimeZone;
    let meta = prep.meta(path).cloned().unwrap_or_default();
    let doc = prep.vocab.docs.get(&OcrIndex::key(path));
    let (pw, ph, colors) = match prep.colors.get(&HashStore::key(path)) {
        Some(((pw, ph, _), c)) => (*pw, *ph, *c),
        None => (w, h, 0),
    };
    let t = chrono::Local.timestamp_millis_opt(time_ms as i64).single().map(|d| d.naive_local()).unwrap_or(prep.now);
    let (when, hours) = time_bits(t, prep.now);
    let mut other = 0u8;
    if meta.src == "long" || (pw > 0 && ph >= pw.saturating_mul(2) && ph >= 1600) {
        other |= OTHER_LONG;
    }
    if meta.src == "edit" || name.contains("_編輯") {
        other |= OTHER_EDITED;
    }
    if meta.src == "frame" {
        other |= OTHER_FRAME;
    }
    if prep.pins.contains(&path.to_lowercase()) {
        other |= OTHER_STARRED;
    }
    if doc.is_some_and(|d| d.has_text) {
        other |= OTHER_TEXT;
    }
    Facets {
        kinds: doc.map(|d| d.kinds).unwrap_or(0) | app_kinds(&meta.app),
        terms: doc.map(|d| d.terms.clone()).unwrap_or_default(),
        app: meta.app,
        when,
        hours,
        colors,
        other,
        day: t.format("%Y-%m-%d").to_string(),
        read: doc.is_some(),
        folder: String::new(),
    }
}

/// 子資料夾顯示的名稱（空的 = 儲存位置本身）
pub fn folder_label(folder: &str) -> String {
    if folder.is_empty() {
        tr!("（不在子資料夾）", "(Top level)").to_string()
    } else {
        folder.replace(['\\', '/'], tr!("／", " / "))
    }
}

// ───────────── 篩選與張數 ─────────────

fn widen(t: &[(u8, &'static str)]) -> Vec<(u16, &'static str)> {
    t.iter().map(|(b, id)| (u16::from(*b), *id)).collect()
}

#[derive(Clone, Copy, PartialEq)]
enum Group {
    Folders,
    Apps,
    Kinds,
    Terms,
    When,
    Hours,
    Colors,
    Other,
    Day,
}

const GROUPS: [Group; 9] = [Group::Folders, Group::Apps, Group::Kinds, Group::Terms, Group::When, Group::Hours, Group::Colors, Group::Other, Group::Day];

fn bit_of<T: Copy + Into<u16>>(table: &[(T, &str)], id: &str) -> u16 {
    table.iter().find(|(_, x)| *x == id).map(|(b, _)| (*b).into()).unwrap_or(0)
}

/// 選的條件換成好比對的形式
struct Compiled {
    folders: HashSet<String>,
    apps: HashSet<String>,
    kinds: u16,
    terms: Vec<Option<u32>>,
    when: u16,
    hours: u16,
    colors: u16,
    other: u16,
    day: Option<String>,
}

fn compile(q: &FacetQuery, vocab: &Vocab) -> Compiled {
    let index: HashMap<&str, u32> = vocab.terms.iter().enumerate().map(|(i, t)| (t.text.as_str(), i as u32)).collect();
    let mask = |ids: &[String], f: &dyn Fn(&str) -> u16| ids.iter().fold(0u16, |m, id| m | f(id));
    Compiled {
        folders: q.folders.iter().cloned().collect(),
        apps: q.apps.iter().cloned().collect(),
        kinds: mask(&q.kinds, &|id| bit_of(&KINDS, id)),
        terms: q.terms.iter().map(|t| index.get(t.as_str()).copied()).collect(),
        when: mask(&q.when, &|id| bit_of(&WHENS, id)),
        hours: mask(&q.hours, &|id| bit_of(&HOURS, id)),
        colors: mask(&q.colors, &|id| bit_of(&COLORS, id)),
        other: mask(&q.other, &|id| bit_of(&OTHERS, id)),
        day: q.day.clone(),
    }
}

fn group_ok(c: &Compiled, f: &Facets, g: Group) -> bool {
    match g {
        Group::Folders => c.folders.is_empty() || c.folders.contains(&f.folder),
        Group::Apps => c.apps.is_empty() || c.apps.contains(&f.app),
        Group::Kinds => c.kinds == 0 || f.kinds & c.kinds != 0,
        // 常見詞：全部都要有（認不得的詞算沒有）
        Group::Terms => c.terms.iter().all(|t| t.is_some_and(|t| f.terms.binary_search(&t).is_ok())),
        Group::When => c.when == 0 || u16::from(f.when) & c.when != 0,
        Group::Hours => c.hours == 0 || u16::from(f.hours) & c.hours != 0,
        Group::Colors => c.colors == 0 || u16::from(f.colors) & c.colors != 0,
        Group::Other => c.other == 0 || u16::from(f.other) & c.other != 0,
        Group::Day => c.day.as_ref().is_none_or(|d| *d == f.day),
    }
}

fn ok_except(c: &Compiled, f: &Facets, skip: Option<Group>) -> bool {
    GROUPS.iter().all(|g| Some(*g) == skip || group_ok(c, f, *g))
}

/// 依條件篩選（回傳每一張要不要留），並算出瀏覽欄每個選項的張數：
/// 同一組的選項張數不受這一組自己已選的影響（選了 Chrome 還看得到 Edge 有幾張），常見詞則是加上去後還剩幾張
pub fn filter_and_count(items: &[Facets], q: &FacetQuery, prep: &Prepared) -> (Vec<bool>, FacetCounts) {
    let c = &compile(q, &prep.vocab);
    let keep: Vec<bool> = items.iter().map(|f| ok_except(c, f, None)).collect();
    let base = |g: Group| items.iter().filter(move |f| ok_except(c, f, Some(g)));

    let opts = |pairs: Vec<(String, String, usize)>, selected: &[String]| -> Vec<FacetOption> {
        pairs.into_iter().filter(|(id, _, n)| *n > 0 || selected.contains(id)).map(|(id, label, count)| FacetOption { id, label, count }).collect()
    };
    let bits = |table: &[(u16, &str)], g: Group, get: &dyn Fn(&Facets) -> u16, label: &dyn Fn(&str) -> String, selected: &[String]| {
        let pairs = table.iter().map(|(b, id)| (id.to_string(), label(id), base(g).filter(|f| get(f) & b != 0).count())).collect();
        opts(pairs, selected)
    };

    // 子資料夾：只有一個資料夾時不列；儲存位置本身在最前面，其他依名稱
    let mut folders: HashMap<String, usize> = HashMap::new();
    for f in base(Group::Folders) {
        *folders.entry(f.folder.clone()).or_default() += 1;
    }
    for d in &q.folders {
        folders.entry(d.clone()).or_default();
    }
    let all_folders: HashSet<&str> = items.iter().map(|f| f.folder.as_str()).collect();
    let folders: Vec<FacetOption> = if all_folders.len() < 2 {
        vec![]
    } else {
        let mut v: Vec<(String, usize)> = folders.into_iter().collect();
        v.sort_by(|a, b| (!a.0.is_empty()).cmp(&!b.0.is_empty()).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase())));
        v.into_iter().map(|(id, count)| FacetOption { label: folder_label(&id), id, count }).collect()
    };

    // 程式：依張數由多到少
    let mut apps: HashMap<String, usize> = HashMap::new();
    for f in base(Group::Apps).filter(|f| !f.app.is_empty()) {
        *apps.entry(f.app.clone()).or_default() += 1;
    }
    for a in &q.apps {
        apps.entry(a.clone()).or_default();
    }
    let mut apps: Vec<(String, usize)> = apps.into_iter().collect();
    apps.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let apps = apps.into_iter().map(|(id, count)| FacetOption { label: crate::shot_meta::app_label(&id), id, count }).collect();

    // 常見詞：從分數最高的詞裡挑目前這些截圖裡有的，依張數排；已選的放前面
    let now: Vec<&Facets> = items.iter().zip(&keep).filter(|(_, k)| **k).map(|(f, _)| f).collect();
    let mut tcount: HashMap<u32, usize> = HashMap::new();
    for f in &now {
        for t in &f.terms {
            *tcount.entry(*t).or_default() += 1;
        }
    }
    let mut terms: Vec<FacetOption> = q
        .terms
        .iter()
        .map(|t| FacetOption { id: t.clone(), label: t.clone(), count: now.len() })
        .chain(
            prep.vocab
                .terms
                .iter()
                .enumerate()
                .take(400)
                .filter(|(i, t)| tcount.get(&(*i as u32)).is_some_and(|n| *n >= 2 && *n < now.len().max(2)) && !q.terms.contains(&t.text))
                .map(|(i, t)| FacetOption { id: t.text.clone(), label: t.text.clone(), count: tcount[&(i as u32)] })
                .take(30),
        )
        .collect();
    let selected = q.terms.len();
    terms[selected..].sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.id.cmp(&b.id)));

    let counts = FacetCounts {
        folders,
        apps,
        kinds: bits(&KINDS, Group::Kinds, &|f| f.kinds, &kind_label, &q.kinds),
        terms,
        when: bits(&widen(&WHENS), Group::When, &|f| f.when.into(), &when_label, &q.when),
        hours: bits(&widen(&HOURS), Group::Hours, &|f| f.hours.into(), &hour_label, &q.hours),
        colors: bits(&widen(&COLORS), Group::Colors, &|f| f.colors.into(), &color_label, &q.colors),
        other: bits(&widen(&OTHERS), Group::Other, &|f| f.other.into(), &other_label, &q.other),
        pending_text: items.iter().filter(|f| !f.read).count(),
    };
    (keep, counts)
}

// ───────────── 相關截圖、月曆 ─────────────

/// 和 target 相關的程度：同一個程式、時間接近、有共同的詞、長得像
pub fn related_score(prep: &Prepared, a: (&Facets, f64, Option<crate::similar::Print>), b: (&Facets, f64, Option<crate::similar::Print>)) -> f64 {
    let (fa, ta, pa) = a;
    let (fb, tb, pb) = b;
    let mut s = 0.0;
    if !fa.app.is_empty() && fa.app == fb.app {
        s += 2.0;
    }
    let gap = (ta - tb).abs() / 60_000.0;
    s += if gap < 10.0 {
        3.0
    } else if gap < 60.0 {
        2.0
    } else if fa.day == fb.day {
        1.0
    } else {
        0.0
    };
    // 共同的詞：越少見的詞越有份量（最多 6 分）
    let n = prep.vocab.n.max(1) as f64;
    let mut shared = 0.0;
    let (mut i, mut j) = (0, 0);
    while i < fa.terms.len() && j < fb.terms.len() {
        match fa.terms[i].cmp(&fb.terms[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                let df = prep.vocab.terms.get(fa.terms[i] as usize).map(|t| t.df).unwrap_or(1) as f64;
                shared += (n / df).ln().max(0.2) * 0.5;
                i += 1;
                j += 1;
            }
        }
    }
    s += shared.min(6.0);
    if let (Some((wa, ha, ha_)), Some((wb, hb, hb_))) = (pa, pb) {
        let diff: u32 = ha_.iter().zip(hb_.iter()).map(|(x, y)| (x ^ y).count_ones()).sum();
        if wa == wb && ha == hb && diff <= 20 {
            s += 3.0;
        }
    }
    s
}

/// 兩個詞有沒有共同的兩個字（或一個包含另一個）
fn overlaps(a: &str, b: &str) -> bool {
    if a.contains(b) || b.contains(a) {
        return true;
    }
    let ca: Vec<char> = a.chars().collect();
    let cb: Vec<char> = b.chars().collect();
    ca.len() >= 2 && cb.len() >= 2 && ca.windows(2).any(|w| cb.windows(2).any(|x| x == w))
}

/// 月曆的一天：張數與那天的常見詞（最多 3 個）
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CalDay {
    pub day: u32,
    pub count: usize,
    pub terms: Vec<String>,
}

/// 某個月每一天的張數與常見詞。items：(分類, 時間)
pub fn calendar(prep: &Prepared, items: &[Facets], year: i32, month: u32) -> Vec<CalDay> {
    let prefix = format!("{year:04}-{month:02}-");
    let mut days: HashMap<u32, (usize, HashMap<u32, usize>)> = HashMap::new();
    for f in items.iter().filter(|f| f.day.starts_with(&prefix)) {
        let d: u32 = f.day[8..].parse().unwrap_or(0);
        let e = days.entry(d).or_default();
        e.0 += 1;
        for t in &f.terms {
            *e.1.entry(*t).or_default() += 1;
        }
    }
    let n = prep.vocab.n.max(1) as f64;
    let mut out: Vec<CalDay> = days
        .into_iter()
        .map(|(day, (count, tc))| {
            let mut ts: Vec<(f64, &str)> = tc
                .iter()
                .filter_map(|(id, c)| {
                    let t = prep.vocab.terms.get(*id as usize)?;
                    Some((*c as f64 * (n / t.df as f64).ln().max(0.1), t.text.as_str()))
                })
                .collect();
            ts.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(b.1)));
            // 互相重疊的片段（「明天會議」「天會議幾」）只留一個
            let mut picked: Vec<String> = vec![];
            for (_, t) in ts {
                if picked.len() >= 3 {
                    break;
                }
                if !picked.iter().any(|p| overlaps(p, t)) {
                    picked.push(t.to_string());
                }
            }
            CalDay { day, count, terms: picked }
        })
        .collect();
    out.sort_by_key(|d| d.day);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_from_text_and_app() {
        let n = crate::ocr_index::normalize;
        assert!(text_kinds(&n("502 Bad Gateway error occurred while loading")) & KIND_ERROR != 0);
        assert!(text_kinds(&n("請到 https://example.com 查看訂單 NT$ 1,200")) & (KIND_WEB | KIND_MONEY) == KIND_WEB | KIND_MONEY);
        assert!(text_kinds(&n("小明 下午 3:05 好的 下午 3:06 已讀")) & KIND_CHAT != 0);
        assert!(text_kinds(&n("聯絡 abc@example.com 0912-345-678")) & KIND_PERSONAL != 0);
        assert_eq!(text_kinds("ok"), KIND_NOTEXT);
        assert_eq!(app_kinds("line.exe"), KIND_CHAT);
    }

    #[test]
    fn vocab_keeps_distinctive_longer_terms() {
        let docs: Vec<(String, String)> = vec![
            ("a".into(), "客戶 報價單 第一版 invoice total".into()),
            ("b".into(), "報價單 修改 客戶 invoice".into()),
            ("c".into(), "報價單 確認".into()),
            ("d".into(), "會議 記錄 meeting notes".into()),
            ("e".into(), "會議 記錄 meeting".into()),
            ("f".into(), "天氣".into()),
            ("g".into(), "".into()),
        ];
        let v = build_vocab(&docs);
        let texts: Vec<&str> = v.terms.iter().map(|t| t.text.as_str()).collect();
        assert!(texts.contains(&"報價單") && !texts.contains(&"報價"), "{texts:?}");
        assert!(texts.contains(&"invoice") && texts.contains(&"meeting"));
        assert!(!texts.contains(&"天氣"), "只出現一張的不算");
        assert_eq!(v.n, 6);
        assert_eq!(suggest(&v, "報", 5)[0], ("報價單".to_string(), 3));
        assert!(synonyms("錯誤").contains(&"error".to_string()));
    }

    #[test]
    fn filter_counts_ignore_own_group() {
        let docs: Vec<(String, String)> = vec![("x".into(), "報價單 客戶".into()), ("y".into(), "報價單 客戶".into())];
        let prep = Prepared { vocab: Arc::new(build_vocab(&docs)), meta: HashMap::new(), colors: HashMap::new(), pins: HashSet::new(), now: chrono::Local::now().naive_local() };
        let f = |app: &str, kinds: u16| Facets { app: app.into(), kinds, day: "2026-10-08".into(), ..Default::default() };
        let items = vec![f("chrome.exe", KIND_WEB), f("chrome.exe", KIND_WEB | KIND_ERROR), f("line.exe", KIND_CHAT)];
        let q = FacetQuery { apps: vec!["chrome.exe".into()], ..Default::default() };
        let (keep, c) = filter_and_count(&items, &q, &prep);
        assert_eq!(keep, vec![true, true, false]);
        // 選了 Chrome 還看得到 LINE 有 1 張
        assert_eq!(c.apps.iter().map(|o| (o.id.as_str(), o.count)).collect::<Vec<_>>(), vec![("chrome.exe", 2), ("line.exe", 1)]);
        // 內容類型的張數是在 Chrome 裡面算
        assert_eq!(c.kinds.iter().find(|o| o.id == "error").unwrap().count, 1);
        assert!(!c.kinds.iter().any(|o| o.id == "chat"));
        let q2 = FacetQuery { day: Some("2026-10-09".into()), ..Default::default() };
        assert!(filter_and_count(&items, &q2, &prep).0.iter().all(|k| !k));
    }

    #[test]
    fn time_ranges() {
        let now = chrono::NaiveDate::from_ymd_opt(2026, 10, 15).unwrap().and_hms_opt(14, 0, 0).unwrap();
        let at = |m, d, h| chrono::NaiveDate::from_ymd_opt(2026, m, d).unwrap().and_hms_opt(h, 0, 0).unwrap();
        assert_eq!(time_bits(at(10, 15, 9), now), (1 | 4 | 8, 1));
        assert_eq!(time_bits(at(10, 14, 20), now), (2 | 4 | 8, 4));
        assert_eq!(time_bits(at(9, 30, 13), now), (16, 2));
        assert_eq!(time_bits(at(8, 1, 13), now).0, 32);
    }

    #[test]
    fn folders_group_lists_only_when_more_than_one() {
        let t = tempfile::tempdir().unwrap();
        let src_ocr = crate::ocr_index::OcrIndex::new(t.path().join("ocr.json"));
        let meta = ShotMetaStore::new(t.path().join("meta.json"));
        let hashes = HashStore::new(t.path().join("hash.json"));
        let pins = PinStore::new(t.path().join("pins.json"));
        let vocab = VocabCache::default();
        let prep = prepare(&Sources { ocr: &src_ocr, meta: &meta, hashes: &hashes, pins: &pins, vocab: &vocab });
        let f = |folder: &str| Facets { folder: folder.into(), day: "2026-10-08".into(), ..Default::default() };
        let one = [f(""), f("")];
        let (_, c) = filter_and_count(&one, &FacetQuery::default(), &prep);
        assert!(c.folders.is_empty());
        let items = [f(""), f("教學"), f("教學"), f("會議")];
        let (keep, c) = filter_and_count(&items, &FacetQuery::default(), &prep);
        assert_eq!(keep, [true; 4]);
        // 最上層在最前面，其他依名稱
        assert_eq!(c.folders.iter().map(|o| (o.id.as_str(), o.count)).collect::<Vec<_>>(), [("", 1), ("教學", 2), ("會議", 1)]);
        let (keep, c) = filter_and_count(&items, &FacetQuery { folders: vec!["教學".into()], ..Default::default() }, &prep);
        assert_eq!(keep, [false, true, true, false]);
        // 同一組的張數不受自己已選的影響
        assert_eq!(c.folders.len(), 3);
    }
}
